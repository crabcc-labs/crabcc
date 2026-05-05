"""Decorator that wraps a tool fn with an approval round-trip.

The OpenAI Agents SDK derives each tool's JSON schema from its wrapped
function's signature + docstring. We use ``functools.wraps`` so the
gated wrapper preserves both — the model still sees the underlying
function's parameters, types, and docs.

Risk classes:

* ``auto`` — bypass the gate; the tool runs unconditionally. Used for
  read-only crabcc lookups (sym/refs/callers/files/outline/fuzzy)
  and read-only memory ops (memory_search, memory_list).
* ``required`` — always send a Telegram prompt; deny the call on
  timeout or explicit deny. Used for write-ish or
  side-effecting tools (memory_remember, fetch_url).

The split is configurable per-deployment via env: a tool listed in
``CRABCC_HITL_APPROVAL_REQUIRED`` always asks; one in
``CRABCC_HITL_APPROVAL_AUTO`` always bypasses. Override-list-vs-default
is computed by :class:`Settings.gate_for`, called at app startup.
"""

from __future__ import annotations

import functools
import logging
from collections.abc import Awaitable, Callable
from typing import TYPE_CHECKING, Any, Literal

from .approvals import Decision, PendingApprovals, current_chat_id
from .tools._mcp_client import McpToolResult

if TYPE_CHECKING:
    from ._types import InlineKeyboardMarkup
    from .approvals import ApprovalRequest
    from .telegram_client import TelegramBotClient

logger = logging.getLogger(__name__)


RiskKind = Literal["auto", "required"]


# Module-level wires set in lifespan. Tools call into the gate via
# functions, not via app.state — the Agents SDK calls tool fns
# without a request context, so we can't reach FastAPI state.
_pending: PendingApprovals | None = None
_telegram: TelegramBotClient | None = None
_default_chat_id: int | None = None
_default_timeout_s: float = 60.0


def configure(
    *,
    pending: PendingApprovals,
    telegram: TelegramBotClient | None,
    default_chat_id: int | None,
    default_timeout_s: float,
) -> None:
    """Wire the gate to its dependencies. Called once from the lifespan.

    ``telegram`` may be ``None`` — in that case ``required`` tools fail
    closed (decision = deny) so an unconfigured deploy can't accidentally
    let an agent execute side-effecting tools without a human.
    """
    global _pending, _telegram, _default_chat_id, _default_timeout_s
    _pending = pending
    _telegram = telegram
    _default_chat_id = default_chat_id
    _default_timeout_s = default_timeout_s


def _denied(tool: str, reason: str) -> McpToolResult:
    return McpToolResult(ok=False, tool=tool, error=f"approval denied: {reason}")


def _build_keyboard(request_id: str) -> InlineKeyboardMarkup:
    """Inline keyboard with Approve / Deny callback_data buttons.

    Telegram's callback_data caps at 64 bytes — our request ids are
    URL-safe 16-char tokens so the longest payload is
    ``"approve:<16-char>"`` ≈ 24 bytes, comfortably under.
    """
    return {
        "inline_keyboard": [
            [
                {"text": "✅ Approve", "callback_data": f"approve:{request_id}"},
                {"text": "🛑 Deny", "callback_data": f"deny:{request_id}"},
            ]
        ]
    }


def _format_prompt(tool: str, arguments: dict[str, Any]) -> str:
    """Pretty-print a tool call for the Telegram prompt.

    Truncates each value at 200 chars so a giant arg blob can't blow
    the 4096-char message cap.
    """
    lines = [f"🔧 Agent wants to call `{tool}`"]
    if not arguments:
        lines.append("(no arguments)")
    else:
        lines.append("Arguments:")
        for k, v in arguments.items():
            s = str(v)
            if len(s) > 200:
                s = s[:200] + "…"
            lines.append(f"• `{k}` = `{s}`")
    return "\n".join(lines)


def gated(
    fn: Callable[..., Awaitable[McpToolResult]],
    *,
    risk: RiskKind,
) -> Callable[..., Awaitable[McpToolResult]]:
    """Wrap a tool function with the approval gate.

    Preserves the wrapped function's name, signature, and docstring so
    the Agents SDK's ``function_tool`` derives the correct schema and
    the model addresses it by the original name.
    """
    tool_name = fn.__name__

    @functools.wraps(fn)
    async def wrapper(**kwargs: Any) -> McpToolResult:
        if risk == "auto":
            return await fn(**kwargs)

        if _pending is None:
            # Mis-configured: fail closed so we don't silently bypass.
            logger.error("tool gate not configured; denying %s", tool_name)
            return _denied(tool_name, "gate not configured")

        chat_id = current_chat_id.get() or _default_chat_id
        if _telegram is None or chat_id is None:
            logger.warning(
                "no telegram channel for approval; denying %s",
                tool_name,
                extra={"chat_id": chat_id, "telegram_set": _telegram is not None},
            )
            return _denied(tool_name, "no operator channel")

        async def send_prompt(item: ApprovalRequest) -> None:
            """Side-effect supplied to PendingApprovals.request().

            Sending the prompt *after* the entry is registered avoids a
            race where a fast-clicking user's callback arrives before
            the registry knows about the request.
            """
            assert _telegram is not None  # narrows for mypy
            await _telegram.send_message(
                chat_id=chat_id,
                text=_format_prompt(tool_name, kwargs),
                reply_markup=_build_keyboard(item.id),
                parse_mode="Markdown",
            )

        decision: Decision = await _pending.request(
            tool=tool_name,
            arguments=kwargs,
            chat_id=chat_id,
            timeout_s=_default_timeout_s,
            on_registered=send_prompt,
        )
        if decision.kind == "approve":
            return await fn(**kwargs)
        return _denied(tool_name, decision.reason or "user denied")

    return wrapper
