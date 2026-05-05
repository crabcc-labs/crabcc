"""Phase 2 — approval flow + Mini App auth."""

from __future__ import annotations

import asyncio
import hashlib
import hmac
import urllib.parse
from typing import Any

import pytest

from crabcc_hitl.approvals import Decision, PendingApprovals, current_chat_id
from crabcc_hitl.telegram_client import validate_init_data
from crabcc_hitl.tool_gate import configure as configure_tool_gate
from crabcc_hitl.tool_gate import gated
from crabcc_hitl.tools._mcp_client import McpToolResult

# ───── PendingApprovals — registry primitives ─────


@pytest.mark.asyncio
async def test_request_resolves_on_response() -> None:
    pending = PendingApprovals(default_timeout_s=5.0)

    captured_request: dict[str, Any] = {}

    async def on_registered(item: Any) -> None:
        captured_request["id"] = item.id

    request_task = asyncio.create_task(
        pending.request(
            tool="memory_remember",
            arguments={"key": "k", "body": "v"},
            chat_id=42,
            on_registered=on_registered,
        )
    )
    # Yield once so the task registers and on_registered fires.
    await asyncio.sleep(0)
    assert "id" in captured_request
    assert len(pending) == 1

    accepted = pending.respond(captured_request["id"], Decision(kind="approve"))
    assert accepted is True

    decision = await request_task
    assert decision.kind == "approve"
    assert len(pending) == 0  # cleaned up


@pytest.mark.asyncio
async def test_request_times_out_into_deny() -> None:
    pending = PendingApprovals(default_timeout_s=0.05)
    decision = await pending.request(
        tool="fetch_url",
        arguments={"url": "https://example.com"},
        chat_id=42,
    )
    assert decision.kind == "deny"
    assert decision.reason == "timeout"
    assert len(pending) == 0


@pytest.mark.asyncio
async def test_unknown_request_id_is_noop() -> None:
    pending = PendingApprovals()
    accepted = pending.respond("nope", Decision(kind="approve"))
    assert accepted is False


@pytest.mark.asyncio
async def test_double_response_is_noop() -> None:
    pending = PendingApprovals(default_timeout_s=5.0)

    seen_id: dict[str, str] = {}

    async def on_registered(item: Any) -> None:
        seen_id["id"] = item.id

    task = asyncio.create_task(
        pending.request(
            tool="x",
            arguments={},
            chat_id=1,
            on_registered=on_registered,
        )
    )
    await asyncio.sleep(0)
    request_id = seen_id["id"]
    assert pending.respond(request_id, Decision(kind="approve")) is True
    # Wait for the request to settle so the registry is cleaned up.
    await task
    # Second response after the entry is gone — no-op.
    assert pending.respond(request_id, Decision(kind="deny")) is False


@pytest.mark.asyncio
async def test_prompt_failure_denies_immediately() -> None:
    pending = PendingApprovals(default_timeout_s=5.0)

    async def on_registered(_item: Any) -> None:
        raise RuntimeError("telegram down")

    decision = await pending.request(
        tool="x",
        arguments={},
        chat_id=1,
        on_registered=on_registered,
    )
    assert decision.kind == "deny"
    assert "telegram down" in (decision.reason or "")


# ───── tool_gate — gated() decorator ─────


async def _fake_tool(arg: str = "default") -> McpToolResult:
    """Stand-in for a real tool — returns the args echoed back."""
    return McpToolResult(ok=True, tool="_fake_tool", content={"echo": arg})


@pytest.mark.asyncio
async def test_auto_risk_bypasses_gate() -> None:
    pending = PendingApprovals(default_timeout_s=5.0)
    configure_tool_gate(
        pending=pending,
        telegram=None,  # not configured, but auto bypasses anyway
        default_chat_id=None,
        default_timeout_s=5.0,
    )
    g = gated(_fake_tool, risk="auto")
    res = await g(arg="hello")
    assert res.ok is True
    assert res.content == {"echo": "hello"}
    assert len(pending) == 0  # never registered


@pytest.mark.asyncio
async def test_required_risk_without_telegram_denies() -> None:
    """Required tools must fail closed when telegram isn't wired."""
    pending = PendingApprovals(default_timeout_s=5.0)
    configure_tool_gate(
        pending=pending,
        telegram=None,
        default_chat_id=None,
        default_timeout_s=5.0,
    )
    g = gated(_fake_tool, risk="required")
    res = await g(arg="hello")
    assert res.ok is False
    assert "no operator channel" in (res.error or "")


@pytest.mark.asyncio
async def test_required_risk_approve_runs_tool() -> None:
    pending = PendingApprovals(default_timeout_s=5.0)
    sent: list[dict[str, Any]] = []

    class _FakeTelegram:
        async def send_message(self, **kwargs: Any) -> dict[str, Any]:
            sent.append(kwargs)
            return {}

    configure_tool_gate(
        pending=pending,
        telegram=_FakeTelegram(),  # type: ignore[arg-type]
        default_chat_id=42,
        default_timeout_s=5.0,
    )
    g = gated(_fake_tool, risk="required")

    # Run the tool and approve in parallel.
    async def approve_after_register() -> None:
        # Wait until the entry shows up in the registry.
        for _ in range(50):
            await asyncio.sleep(0.005)
            items = pending.list()
            if items:
                pending.respond(items[0].id, Decision(kind="approve"))
                return
        pytest.fail("approval entry never registered")

    res, _ = await asyncio.gather(g(arg="hello"), approve_after_register())
    assert res.ok is True
    assert res.content == {"echo": "hello"}
    # Verify the prompt went out with both buttons.
    assert len(sent) == 1
    assert sent[0]["chat_id"] == 42
    keyboard = sent[0]["reply_markup"]["inline_keyboard"]
    assert keyboard[0][0]["callback_data"].startswith("approve:")
    assert keyboard[0][1]["callback_data"].startswith("deny:")


@pytest.mark.asyncio
async def test_required_risk_deny_short_circuits() -> None:
    pending = PendingApprovals(default_timeout_s=5.0)

    class _FakeTelegram:
        async def send_message(self, **_kwargs: Any) -> dict[str, Any]:
            return {}

    configure_tool_gate(
        pending=pending,
        telegram=_FakeTelegram(),  # type: ignore[arg-type]
        default_chat_id=42,
        default_timeout_s=5.0,
    )
    tool_called = False

    async def tracking_tool() -> McpToolResult:
        nonlocal tool_called
        tool_called = True
        return McpToolResult(ok=True, tool="tracking_tool")

    g = gated(tracking_tool, risk="required")

    async def deny_after_register() -> None:
        for _ in range(50):
            await asyncio.sleep(0.005)
            items = pending.list()
            if items:
                pending.respond(items[0].id, Decision(kind="deny", reason="nope"))
                return
        pytest.fail("approval entry never registered")

    res, _ = await asyncio.gather(g(), deny_after_register())
    assert res.ok is False
    assert "nope" in (res.error or "")
    assert tool_called is False


@pytest.mark.asyncio
async def test_chat_id_threaded_via_contextvar() -> None:
    """When current_chat_id is set in context, gate prefers it over default."""
    pending = PendingApprovals(default_timeout_s=5.0)
    sent: list[dict[str, Any]] = []

    class _FakeTelegram:
        async def send_message(self, **kwargs: Any) -> dict[str, Any]:
            sent.append(kwargs)
            return {}

    configure_tool_gate(
        pending=pending,
        telegram=_FakeTelegram(),  # type: ignore[arg-type]
        default_chat_id=99,  # fallback if no contextvar
        default_timeout_s=5.0,
    )
    g = gated(_fake_tool, risk="required")

    token = current_chat_id.set(7777)
    try:

        async def approve_after_register() -> None:
            for _ in range(50):
                await asyncio.sleep(0.005)
                items = pending.list()
                if items:
                    pending.respond(items[0].id, Decision(kind="approve"))
                    return
            pytest.fail("approval entry never registered")

        await asyncio.gather(g(), approve_after_register())
    finally:
        current_chat_id.reset(token)

    assert len(sent) == 1
    assert sent[0]["chat_id"] == 7777  # contextvar wins over default 99


# ───── validate_init_data — Mini App auth ─────


def _sign_init_data(fields: dict[str, str], bot_token: str) -> str:
    """Build a valid Telegram initData payload for tests.

    Mirrors the spec at
    https://core.telegram.org/bots/webapps#validating-data-received-via-the-mini-app
    so the test catches drift in either direction.
    """
    data_check = "\n".join(f"{k}={v}" for k, v in sorted(fields.items()))
    secret_key = hmac.new(b"WebAppData", bot_token.encode(), hashlib.sha256).digest()
    sig = hmac.new(secret_key, data_check.encode(), hashlib.sha256).hexdigest()
    encoded = urllib.parse.urlencode([*fields.items(), ("hash", sig)])
    return encoded


def test_validate_init_data_accepts_signed_payload() -> None:
    bot_token = "1234:test-token"
    fields = {"auth_date": "1700000000", "user": '{"id":42}', "query_id": "Q"}
    init_data = _sign_init_data(fields, bot_token)

    parsed = validate_init_data(init_data, bot_token)

    assert parsed is not None
    assert parsed["auth_date"] == "1700000000"
    assert parsed["query_id"] == "Q"
    assert "hash" not in parsed  # stripped before return


def test_validate_init_data_rejects_tampered_payload() -> None:
    bot_token = "1234:test-token"
    fields = {"auth_date": "1700000000", "user": '{"id":42}'}
    init_data = _sign_init_data(fields, bot_token)
    # Flip a field after signing.
    tampered = init_data.replace("id%22%3A42", "id%22%3A99")

    parsed = validate_init_data(tampered, bot_token)

    assert parsed is None


def test_validate_init_data_rejects_missing_hash() -> None:
    parsed = validate_init_data("auth_date=1700000000", "1234:test-token")
    assert parsed is None


def test_validate_init_data_rejects_empty() -> None:
    assert validate_init_data("", "1234:test-token") is None
