"""In-process audit log for tool-gate decisions.

Every approve / deny / timeout / policy-bypass / prompt-failure
recorded here, ring-buffered so the service can run for days
without unbounded memory growth. Operators read the trail through
:func:`crabcc_hitl.main.approval_audit` (bearer) or the Mini App's
``/webapp/api/audit`` (initData).

Persistence to the crabcc memory drawer is intentionally a *future*
phase: it requires ``CRABCC_HITL_MCP_BASE_URL`` to be live, and the
``memory_remember`` tool itself goes through this gate — so we'd be
writing audit records via the same path we're auditing. In-process
keeps the dependency graph clean.
"""

from __future__ import annotations

import logging
import time
from collections import deque
from collections.abc import Iterable
from dataclasses import dataclass
from typing import Any, Literal

logger = logging.getLogger(__name__)


# Each value tells the operator *why* a tool ran or didn't.
#  - ``approve`` / ``deny`` — explicit human decision via Telegram.
#  - ``auto`` — risk class is ``auto``, no prompt issued.
#  - ``policy`` — risk is ``required`` but matched the auto-approve
#                 allowlist, so no prompt issued.
#  - ``timeout`` — prompt sent, no response within the window.
#  - ``prompt_failed`` — Telegram REST refused the message; gate
#                        denied to avoid a silent run.
#  - ``misconfigured`` — gate has no operator channel; denied closed.
DecisionSource = Literal[
    "approve", "deny", "auto", "policy", "timeout", "prompt_failed", "misconfigured"
]


@dataclass(frozen=True)
class DecisionRecord:
    """One auditable tool-gate decision."""

    timestamp: float
    tool: str
    arguments: dict[str, Any]
    source: DecisionSource
    chat_id: int | None
    # When ``source == "policy"``, the matched ``tool:arg=glob`` rule
    # text. ``None`` for every other source.
    matched_rule: str | None = None
    # When ``source == "deny"``, the user's explanation if the bot
    # collected one. Always ``None`` when the deny came from the gate
    # itself (timeout / misconfigured).
    reason: str | None = None


class DecisionAudit:
    """Bounded ring-buffer of recent gate decisions.

    Thread-safety: writes happen on the asyncio loop only; the buffer
    is a :class:`collections.deque` whose ``append`` is atomic at the
    Python level. Iteration via :meth:`recent` is safe because we
    snapshot into a list first.
    """

    def __init__(self, *, capacity: int = 200) -> None:
        self._records: deque[DecisionRecord] = deque(maxlen=capacity)
        self._capacity = capacity

    @property
    def capacity(self) -> int:
        return self._capacity

    def __len__(self) -> int:
        return len(self._records)

    def record(  # noqa: PLR0913 — keyword-only; record fields are the public schema
        self,
        *,
        tool: str,
        arguments: dict[str, Any],
        source: DecisionSource,
        chat_id: int | None,
        matched_rule: str | None = None,
        reason: str | None = None,
    ) -> DecisionRecord:
        """Append a record and return it (lets callers log + return in one shot)."""
        rec = DecisionRecord(
            timestamp=time.time(),
            tool=tool,
            arguments=arguments,
            source=source,
            chat_id=chat_id,
            matched_rule=matched_rule,
            reason=reason,
        )
        self._records.append(rec)
        logger.info(
            "gate decision",
            extra={
                "tool": tool,
                "source": source,
                "matched_rule": matched_rule,
                "chat_id": chat_id,
            },
        )
        return rec

    def recent(self, limit: int | None = None) -> list[DecisionRecord]:
        """Snapshot newest-first. ``limit`` clips; ``None`` returns all."""
        snapshot = list(self._records)
        snapshot.reverse()
        if limit is None:
            return snapshot
        return snapshot[: max(0, limit)]

    def extend(self, records: Iterable[DecisionRecord]) -> None:
        """Bulk append (used by tests; production callers use :meth:`record`)."""
        for r in records:
            self._records.append(r)
