"""Unit tests for upstream liveness probes."""

from __future__ import annotations

import httpx
import pytest

from crabcc_hitl.probes import probe_crabcc_mcp


@pytest.mark.asyncio
async def test_crabcc_mcp_skipped_when_unconfigured() -> None:
    async with httpx.AsyncClient() as client:
        result = await probe_crabcc_mcp(client, None, timeout_s=1.0)
    assert result.status == "skipped"
    assert result.required is False


@pytest.mark.asyncio
async def test_crabcc_mcp_required_when_configured() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        if request.url.host == "mcp.test":
            return httpx.Response(200, json={"status": "ok"})
        return httpx.Response(503)

    transport = httpx.MockTransport(handler)
    async with httpx.AsyncClient(transport=transport) as client:
        ok = await probe_crabcc_mcp(client, "http://mcp.test", timeout_s=1.0)
        fail = await probe_crabcc_mcp(client, "http://127.0.0.1:1", timeout_s=0.5)
    assert ok.status == "ok"
    assert ok.required is True
    assert fail.status == "fail"
    assert fail.required is True
