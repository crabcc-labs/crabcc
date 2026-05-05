"""Tests for the Phase 1 crabcc + memory tools.

Each tool is a thin wrapper around ``call_tool`` — verifying:
1. Without ``CRABCC_HITL_MCP_BASE_URL`` set, every tool returns
   ``ok=False`` with a clear "MCP not configured" error.
2. With it set, the tool issues the right JSON-RPC body to the
   right URL and unwraps the response correctly.

Test harness mocks httpx so we don't need a real crabcc-mcp server.
"""

from __future__ import annotations

import orjson
import pytest

from crabcc_hitl.tools._mcp_client import call_tool

# ───── Unconfigured path ─────


@pytest.mark.asyncio
async def test_unconfigured_returns_clear_error(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("CRABCC_HITL_MCP_BASE_URL", raising=False)
    res = await call_tool("sym", {"name": "Store"})
    assert res.ok is False
    assert "MCP" in (res.error or "")
    assert res.tool == "sym"


# ───── Configured success path ─────


def _install_fake_httpx(monkeypatch: pytest.MonkeyPatch, payload: object) -> list[dict]:
    """Patch httpx.AsyncClient so calls return ``payload`` as MCP success.

    Returns a list that captures every (url, body) sent — tests assert
    against the call shape after running the tool.
    """
    captured: list[dict] = []

    class _Resp:
        status_code = 200

        def __init__(self, body: bytes) -> None:
            self.content = body

        @property
        def text(self) -> str:
            return self.content.decode()

    class _Client:
        def __init__(self, *_a: object, **_kw: object) -> None:
            pass

        async def __aenter__(self) -> _Client:
            return self

        async def __aexit__(self, *_a: object) -> None:
            return None

        async def post(self, url: str, *, content: bytes, headers: dict) -> _Resp:
            captured.append({"url": url, "body": orjson.loads(content), "headers": headers})
            envelope = {
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "content": [{"type": "text", "text": orjson.dumps(payload).decode()}],
                    "isError": False,
                },
            }
            return _Resp(orjson.dumps(envelope))

    import httpx

    monkeypatch.setattr(httpx, "AsyncClient", _Client)
    return captured


@pytest.mark.asyncio
async def test_configured_decodes_json_text_payload(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("CRABCC_HITL_MCP_BASE_URL", "http://crabcc-mcp:9090")
    monkeypatch.delenv("CRABCC_HITL_MCP_API_TOKEN", raising=False)
    captured = _install_fake_httpx(
        monkeypatch,
        [{"name": "Store", "kind": "struct", "file": "store.rs", "line_start": 12}],
    )

    res = await call_tool("sym", {"name": "Store"})

    assert res.ok is True
    assert isinstance(res.content, list)
    assert res.content[0]["name"] == "Store"
    # Verify wire shape: POST to /mcp with JSON-RPC tools/call envelope.
    assert len(captured) == 1
    sent = captured[0]
    assert sent["url"] == "http://crabcc-mcp:9090/mcp"
    assert sent["body"]["method"] == "tools/call"
    assert sent["body"]["params"]["name"] == "sym"
    assert sent["body"]["params"]["arguments"] == {"name": "Store"}
    assert "Authorization" not in sent["headers"]


@pytest.mark.asyncio
async def test_bearer_auth_attaches(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("CRABCC_HITL_MCP_BASE_URL", "http://mcp:9090")
    monkeypatch.setenv("CRABCC_HITL_MCP_API_TOKEN", "secret-mcp-token")
    captured = _install_fake_httpx(monkeypatch, {"count": 17})

    await call_tool("callers", {"name": "Store::open"})

    sent = captured[0]
    assert sent["headers"].get("Authorization") == "Bearer secret-mcp-token"


@pytest.mark.asyncio
async def test_tool_side_error_returns_ok_false(monkeypatch: pytest.MonkeyPatch) -> None:
    """When MCP server reports `isError: true`, surface as ok=False."""
    monkeypatch.setenv("CRABCC_HITL_MCP_BASE_URL", "http://mcp:9090")
    monkeypatch.delenv("CRABCC_HITL_MCP_API_TOKEN", raising=False)

    class _Resp:
        status_code = 200

        def __init__(self, body: bytes) -> None:
            self.content = body

        @property
        def text(self) -> str:
            return self.content.decode()

    class _Client:
        def __init__(self, *_a: object, **_kw: object) -> None:
            pass

        async def __aenter__(self) -> _Client:
            return self

        async def __aexit__(self, *_a: object) -> None:
            return None

        async def post(self, _url: str, *, content: bytes, headers: dict) -> _Resp:  # noqa: ARG002
            envelope = {
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "content": [{"type": "text", "text": "symbol not found"}],
                    "isError": True,
                },
            }
            return _Resp(orjson.dumps(envelope))

    import httpx

    monkeypatch.setattr(httpx, "AsyncClient", _Client)

    res = await call_tool("sym", {"name": "DoesNotExist"})
    assert res.ok is False
    assert "symbol not found" in (res.error or "")


@pytest.mark.asyncio
async def test_per_tool_thin_wrappers(monkeypatch: pytest.MonkeyPatch) -> None:
    """Each tool module forwards to ``call_tool`` with the right name/args."""
    monkeypatch.setenv("CRABCC_HITL_MCP_BASE_URL", "http://mcp:9090")
    monkeypatch.delenv("CRABCC_HITL_MCP_API_TOKEN", raising=False)
    captured = _install_fake_httpx(monkeypatch, [])

    from crabcc_hitl.tools import (
        crabcc_callers,
        crabcc_files,
        crabcc_fuzzy,
        crabcc_outline,
        crabcc_refs,
        crabcc_sym,
        memory_list,
        memory_remember,
        memory_search,
    )

    await crabcc_sym("X")
    await crabcc_refs("Y", files_only=True, limit=3)
    await crabcc_callers("Z", count_only=True)
    await crabcc_files(under="src", ext="rs")
    await crabcc_outline("a.rs")
    await crabcc_fuzzy("strore")
    await memory_search("query", mode="lexical")
    await memory_remember("k", "body")
    await memory_list(limit=5)

    names = [c["body"]["params"]["name"] for c in captured]
    assert names == [
        "sym",
        "refs",
        "callers",
        "files",
        "outline",
        "fuzzy",
        "memory.search",
        "memory.remember",
        "memory.list",
    ]
    # Spot-check argument forwarding.
    refs_args = captured[1]["body"]["params"]["arguments"]
    assert refs_args == {"name": "Y", "files_only": True, "limit": 3}
    callers_args = captured[2]["body"]["params"]["arguments"]
    assert callers_args == {"name": "Z", "count": True}
