"""In-process pending-approvals registry.

Phase 2 keeps state in-memory: a single-process FastAPI service does
not need to survive restarts for approval semantics — pending requests
just time out and the agent loop reports denial. Persisting them to
Redis would buy little (the agent loop holding the awaiting future is
also in-process) and would fan out a second moving part.

Public surface:

* :class:`PendingApprovals` — registry, owns the futures.
* :class:`ApprovalRequest` — record returned to the Mini App / bot
  (includes ``id``, ``tool``, ``arguments``, ``created_at``,
  ``expires_at``).
* :class:`Decision` — ``approve`` or ``deny`` (+ optional ``reason``).
* :data:`current_chat_id` — :class:`contextvars.ContextVar` threading
  the bot chat-id from ``/chat`` down into the tool gate without
  changing every tool's signature.
"""

from __future__ import annotations

import asyncio
import logging
import secrets
import time
from collections.abc import Awaitable, Callable
from contextvars import ContextVar
from dataclasses import dataclass, field
from typing import Any, Literal

logger = logging.getLogger(__name__)


DecisionKind = Literal["approve", "deny"]


# ContextVar — set by the /chat handler on entry, read by the tool
# gate. None when no bot session is active (e.g. unit tests, MCP
# callers): gate falls back to env-pinned chat id.
current_chat_id: ContextVar[int | None] = ContextVar("crabcc_hitl_chat_id", default=None)


@dataclass(frozen=True)
class Decision:
    kind: DecisionKind
    reason: str | None = None


@dataclass
class ApprovalRequest:
    """A pending tool invocation awaiting human review.

    ``arguments`` is shown to the user verbatim so they can audit what
    the agent wants to do. ``id`` is a URL-safe random token used as
    the inline-button callback payload and as the Mini App row key.
    """

    id: str
    tool: str
    arguments: dict[str, Any]
    chat_id: int | None
    created_at: float
    expires_at: float
    future: asyncio.Future[Decision] = field(repr=False)

    def to_view(self) -> dict[str, Any]:
        """JSON-serialisable shape for /approval/list + Mini App."""
        return {
            "id": self.id,
            "tool": self.tool,
            "arguments": self.arguments,
            "chat_id": self.chat_id,
            "created_at": self.created_at,
            "expires_at": self.expires_at,
            "remaining_s": max(0.0, self.expires_at - time.time()),
        }


class PendingApprovals:
    """Registry of in-flight approval requests.

    Thread-safety note: every method runs on the asyncio event loop.
    No locks are needed because all mutations happen between awaits;
    the dict-touching code is sync.
    """

    def __init__(self, *, default_timeout_s: float = 60.0) -> None:
        self._default_timeout_s = default_timeout_s
        self._items: dict[str, ApprovalRequest] = {}

    def __len__(self) -> int:
        return len(self._items)

    def list(self) -> list[ApprovalRequest]:
        """Snapshot of currently-pending approvals, oldest first."""
        # Drop expired entries opportunistically — they cannot be
        # responded to anymore. The waiter has already woken with a
        # TimeoutError and synthesised a deny decision.
        now = time.time()
        for k in [k for k, v in self._items.items() if v.expires_at < now]:
            self._items.pop(k, None)
        return sorted(self._items.values(), key=lambda r: r.created_at)

    def get(self, request_id: str) -> ApprovalRequest | None:
        return self._items.get(request_id)

    async def request(
        self,
        *,
        tool: str,
        arguments: dict[str, Any],
        chat_id: int | None,
        timeout_s: float | None = None,
        on_registered: Callable[[ApprovalRequest], Awaitable[None]] | None = None,
    ) -> Decision:
        """Register a pending approval and await its resolution.

        Args:
            tool: Tool identifier — e.g. ``"crabcc_sym"``.
            arguments: Argument map passed to the tool. Shown verbatim
                to the user so they can audit intent.
            chat_id: Telegram chat to send the prompt to. ``None``
                means the gate could not derive one (caller decides
                whether to deny or skip-with-warning upstream).
            timeout_s: Per-call override of ``default_timeout_s``.
            on_registered: Optional async callable invoked with the
                fresh :class:`ApprovalRequest` once it's in the
                registry. Lets callers fire the Telegram message
                *after* the entry exists so a fast-clicking user
                can't lose a callback to a missing key.

        Returns:
            The final :class:`Decision`. Times out into a synthetic
            ``deny`` with reason ``"timeout"``.
        """
        timeout = timeout_s if timeout_s is not None else self._default_timeout_s
        request_id = secrets.token_urlsafe(12)
        loop = asyncio.get_running_loop()
        future: asyncio.Future[Decision] = loop.create_future()
        now = time.time()
        item = ApprovalRequest(
            id=request_id,
            tool=tool,
            arguments=arguments,
            chat_id=chat_id,
            created_at=now,
            expires_at=now + timeout,
            future=future,
        )
        self._items[request_id] = item
        log_extra = {
            "request_id": request_id,
            "tool": tool,
            "chat_id": chat_id,
            "timeout_s": timeout,
        }
        logger.info("approval requested", extra=log_extra)
        try:
            if on_registered is not None:
                # Caller-supplied side effect (typically: send Telegram
                # message). If it fails, deny immediately so the agent
                # doesn't sit waiting on a prompt the user never saw.
                try:
                    await on_registered(item)
                except Exception as e:
                    logger.warning(
                        "approval prompt failed: %s", e, extra={"request_id": request_id}
                    )
                    return Decision(kind="deny", reason=f"prompt failed: {e}")
            try:
                return await asyncio.wait_for(future, timeout=timeout)
            except TimeoutError:
                logger.info("approval timed out", extra={"request_id": request_id, "tool": tool})
                return Decision(kind="deny", reason="timeout")
        finally:
            self._items.pop(request_id, None)

    def respond(self, request_id: str, decision: Decision) -> bool:
        """Resolve a pending approval. Returns ``True`` on success.

        Returns ``False`` when the request id is unknown (already
        resolved or never existed) — callers should treat that as a
        no-op rather than a hard error.
        """
        item = self._items.get(request_id)
        if item is None:
            logger.info("approval response for unknown id", extra={"request_id": request_id})
            return False
        if item.future.done():
            # Already resolved — race between two responders. First
            # wins, second is a no-op.
            return False
        item.future.set_result(decision)
        logger.info(
            "approval resolved",
            extra={"request_id": request_id, "decision": decision.kind, "reason": decision.reason},
        )
        return True
