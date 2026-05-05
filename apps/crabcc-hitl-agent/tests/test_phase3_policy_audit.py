"""Phase 3 — per-arg auto-approve policy + audit ring buffer."""

from __future__ import annotations

import asyncio
from typing import Any

import pytest

from crabcc_hitl.approvals import Decision, PendingApprovals
from crabcc_hitl.audit import DecisionAudit
from crabcc_hitl.policy import ApprovalPolicy
from crabcc_hitl.tool_gate import configure as configure_tool_gate
from crabcc_hitl.tool_gate import gated
from crabcc_hitl.tools._mcp_client import McpToolResult

# ───── ApprovalPolicy parser ─────


def test_policy_parses_single_rule() -> None:
    p = ApprovalPolicy.from_env_value("fetch_url:url=https://github.com/**")
    assert p
    matched = p.auto_approves(tool="fetch_url", arguments={"url": "https://github.com/foo/bar"})
    assert matched is not None
    assert matched.pattern == "https://github.com/**"


def test_policy_skips_malformed_rules() -> None:
    p = ApprovalPolicy.from_env_value("garbage,fetch_url:url=ok,no:equals:sign")
    # Only the well-formed rule survives.
    assert p.auto_approves(tool="fetch_url", arguments={"url": "ok"}) is not None
    assert p.auto_approves(tool="garbage", arguments={}) is None


def test_policy_or_semantics_across_rules() -> None:
    p = ApprovalPolicy.from_env_value(
        "fetch_url:url=https://github.com/**,fetch_url:url=https://docs.python.org/*"
    )
    assert p.auto_approves(tool="fetch_url", arguments={"url": "https://github.com/x"}) is not None
    assert (
        p.auto_approves(tool="fetch_url", arguments={"url": "https://docs.python.org/3"})
        is not None
    )
    assert p.auto_approves(tool="fetch_url", arguments={"url": "https://example.com"}) is None


def test_policy_missing_arg_does_not_match() -> None:
    p = ApprovalPolicy.from_env_value("fetch_url:url=https://example.com")
    assert p.auto_approves(tool="fetch_url", arguments={}) is None


def test_policy_empty_string_yields_empty_policy() -> None:
    assert not ApprovalPolicy.from_env_value(None)
    assert not ApprovalPolicy.from_env_value("")


# ───── DecisionAudit ring buffer ─────


def test_audit_ring_buffer_evicts_oldest() -> None:
    a = DecisionAudit(capacity=3)
    for i in range(5):
        a.record(tool=f"t{i}", arguments={}, source="auto", chat_id=None)
    recent = a.recent()
    assert len(recent) == 3
    # Newest first — t4, t3, t2 (t0, t1 evicted).
    assert [r.tool for r in recent] == ["t4", "t3", "t2"]


def test_audit_recent_clamps_negative_limit() -> None:
    a = DecisionAudit(capacity=10)
    a.record(tool="x", arguments={}, source="auto", chat_id=None)
    assert a.recent(limit=-5) == []


def test_audit_records_carry_metadata() -> None:
    a = DecisionAudit(capacity=10)
    a.record(
        tool="fetch_url",
        arguments={"url": "https://github.com/x"},
        source="policy",
        chat_id=42,
        matched_rule="fetch_url:url=https://github.com/**",
    )
    [rec] = a.recent()
    assert rec.source == "policy"
    assert rec.matched_rule == "fetch_url:url=https://github.com/**"
    assert rec.chat_id == 42


# ───── tool_gate end-to-end with policy + audit ─────


async def _approving_tool(arg: str = "x") -> McpToolResult:
    return McpToolResult(ok=True, tool="_approving_tool", content={"echo": arg})


@pytest.mark.asyncio
async def test_policy_match_runs_tool_without_prompt() -> None:
    pending = PendingApprovals(default_timeout_s=5.0)
    audit = DecisionAudit()

    sent: list[dict[str, Any]] = []

    class _RecordingTelegram:
        async def send_message(self, **kwargs: Any) -> None:
            sent.append(kwargs)

    configure_tool_gate(
        pending=pending,
        telegram=_RecordingTelegram(),  # type: ignore[arg-type]
        policy=ApprovalPolicy.from_env_value(
            "_approving_tool:arg=allow-*",
        ),
        audit=audit,
        default_chat_id=42,
        default_timeout_s=5.0,
    )

    g = gated(_approving_tool, risk="required")
    res = await g(arg="allow-this")

    assert res.ok is True
    assert res.content == {"echo": "allow-this"}
    # No Telegram prompt — policy short-circuited.
    assert sent == []
    # Audit got a "policy" record with the matching rule text.
    [rec] = audit.recent()
    assert rec.source == "policy"
    assert rec.matched_rule == "_approving_tool:arg=allow-*"


@pytest.mark.asyncio
async def test_policy_miss_falls_through_to_prompt() -> None:
    pending = PendingApprovals(default_timeout_s=5.0)
    audit = DecisionAudit()

    sent: list[dict[str, Any]] = []

    class _RecordingTelegram:
        async def send_message(self, **kwargs: Any) -> None:
            sent.append(kwargs)

    configure_tool_gate(
        pending=pending,
        telegram=_RecordingTelegram(),  # type: ignore[arg-type]
        policy=ApprovalPolicy.from_env_value("_approving_tool:arg=only-this"),
        audit=audit,
        default_chat_id=42,
        default_timeout_s=5.0,
    )

    g = gated(_approving_tool, risk="required")

    async def approve_after_register() -> None:
        for _ in range(50):
            await asyncio.sleep(0.005)
            items = pending.list()
            if items:
                pending.respond(items[0].id, Decision(kind="approve"))
                return
        pytest.fail("approval entry never registered")

    res, _ = await asyncio.gather(g(arg="not-allowed"), approve_after_register())

    assert res.ok is True
    # Prompt did go out because policy missed.
    assert len(sent) == 1
    # Audit captured the "approve" outcome (not "policy") since the
    # human granted access.
    [rec] = audit.recent()
    assert rec.source == "approve"
    assert rec.matched_rule is None


@pytest.mark.asyncio
async def test_audit_records_each_outcome() -> None:
    pending = PendingApprovals(default_timeout_s=0.05)
    audit = DecisionAudit()
    configure_tool_gate(
        pending=pending,
        telegram=None,  # no Telegram → required tools deny "misconfigured"
        policy=ApprovalPolicy(),
        audit=audit,
        default_chat_id=None,
        default_timeout_s=0.05,
    )
    # Auto-risk records "auto"
    auto_g = gated(_approving_tool, risk="auto")
    await auto_g(arg="x")
    # Required-risk without telegram records "misconfigured"
    req_g = gated(_approving_tool, risk="required")
    await req_g(arg="x")

    sources = [r.source for r in audit.recent()]
    # Newest first.
    assert sources == ["misconfigured", "auto"]
