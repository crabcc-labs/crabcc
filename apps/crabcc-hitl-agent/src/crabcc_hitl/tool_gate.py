"""Decorator that wraps a tool fn with an approval round-trip.

The OpenAI Agents SDK derives each tool's JSON schema from its wrapped
function's signature + docstring. We use ``functools.wraps`` so the
gated wrapper preserves both — the model still sees the underlying
function's parameters, types, and docs.

Risk classes:

* ``auto`` — bypass the gate; the tool runs unconditionally. Used for
  read-only crabcc lookups (sym/refs/callers/files/outline/fuzzy)
  and read-only memory ops (memory_search, memory_list).
* ``required`` — gate consults two layers:
    1. **Policy** — :class:`crabcc_hitl.policy.ApprovalPolicy` glob
       allowlist. A matching rule short-circuits to a "policy"
       audit record and runs the tool.
    2. **Human prompt** — Telegram inline buttons; await response
       (timeout → synthetic deny).

Every outcome is recorded into :class:`crabcc_hitl.audit.DecisionAudit`
with a tagged source so the audit trail explains *why* a tool ran or
didn't.
"""

from __future__ import annotations

import functools
import logging
from collections.abc import Awaitable, Callable
from typing import TYPE_CHECKING, Any, Literal

from .approvals import Decision, PendingApprovals, current_chat_id
from .policy import ApprovalPolicy
from .tools._mcp_client import McpToolResult

if TYPE_CHECKING:
    from ._types import InlineKeyboardMarkup
    from .approvals import ApprovalRequest
    from .audit import DecisionAudit, DecisionSource
    from .telegram_client import TelegramBotClient

logger = logging.getLogger(__name__)


RiskKind = Literal["auto", "required"]


# Module-level wires set in lifespan. Tools call into the gate via
# functions, not via app.state — the Agents SDK calls tool fns
# without a request context, so we can't reach FastAPI state.
_pending: PendingApprovals | None = None
_telegram: TelegramBotClient | None = None
_policy: ApprovalPolicy = ApprovalPolicy()
_audit: DecisionAudit | None = None
_default_chat_id: int | None = None
_default_timeout_s: float = 60.0


def configure(  # noqa: PLR0913 — keyword-only; cohesive single-call wiring beats split helpers
    *,
    pending: PendingApprovals,
    telegram: TelegramBotClient | None,
    policy: ApprovalPolicy,
    audit: DecisionAudit,
    default_chat_id: int | None,
    default_timeout_s: float,
) -> None:
    """Wire the gate to its dependencies. Called once from the lifespan.

    ``telegram`` may be ``None`` — required tools then fail closed so an
    unconfigured deploy can't silently let an agent execute
    side-effecting work without a human.
    """
    global _pending, _telegram, _policy, _audit, _default_chat_id, _default_timeout_s
    _pending = pending
    _telegram = telegram
    _policy = policy
    _audit = audit
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
        chat_id = current_chat_id.get() or _default_chat_id

        if risk == "auto":
            if _audit is not None:
                _audit.record(tool=tool_name, arguments=kwargs, source="auto", chat_id=chat_id)
            return await fn(**kwargs)

        if _pending is None or _audit is None:
            # Mis-configured: fail closed so we don't silently bypass.
            logger.error("tool gate not configured; denying %s", tool_name)
            return _denied(tool_name, "gate not configured")

        # Policy short-circuit — a rule like ``fetch_url:url=https://github.com/**``
        # auto-approves without a human prompt.
        matched = _policy.auto_approves(tool=tool_name, arguments=kwargs)
        if matched is not None:
            rule_text = f"{matched.tool}:{matched.arg}={matched.pattern}"
            _audit.record(
                tool=tool_name,
                arguments=kwargs,
                source="policy",
                chat_id=chat_id,
                matched_rule=rule_text,
            )
            return await fn(**kwargs)

        if _telegram is None or chat_id is None:
            logger.warning(
                "no telegram channel for approval; denying %s",
                tool_name,
                extra={"chat_id": chat_id, "telegram_set": _telegram is not None},
            )
            _audit.record(tool=tool_name, arguments=kwargs, source="misconfigured", chat_id=chat_id)
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
        # Map the decision shape to an audit source. ``timeout`` and
        # ``prompt failed: …`` come back wearing ``deny`` so the audit
        # has to inspect ``reason`` to recover the original cause.
        if decision.kind == "approve":
            _audit.record(tool=tool_name, arguments=kwargs, source="approve", chat_id=chat_id)
            return await fn(**kwargs)
        reason = decision.reason or "user denied"
        source: DecisionSource
        if reason == "timeout":
            source = "timeout"
        elif reason.startswith("prompt failed"):
            source = "prompt_failed"
        else:
            source = "deny"
        _audit.record(
            tool=tool_name,
            arguments=kwargs,
            source=source,
            chat_id=chat_id,
            reason=reason,
        )
        return _denied(tool_name, reason)

    return wrapper
