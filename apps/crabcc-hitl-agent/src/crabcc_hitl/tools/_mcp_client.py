"""Shared MCP-HTTP client for the crabcc + memory tool families.

Phase 1 keeps this small: stateless POSTs against ``/mcp`` with the
JSON-RPC 2.0 envelope MCP defines for ``tools/call``. The crabcc
MCP-HTTP server (started with ``crabcc --mcp-http :PORT --auth-token
$TOKEN``, see issue #204) returns ``{content: [{type:"text", text:
"...JSON..."}], isError: false}``.

When ``CRABCC_HITL_MCP_BASE_URL`` is unset, every tool that imports
from here returns a structured "unavailable" result instead of
raising. Lets the bot keep responding conversationally even when the
crabcc-mcp service isn't running.
"""

from __future__ import annotations

import logging
import os
from typing import Any

import httpx
import orjson
from pydantic import BaseModel, Field

logger = logging.getLogger(__name__)


class McpToolResult(BaseModel):
    """Structured outcome returned by every tool wrapper.

    Agents branch on ``ok`` instead of catching exceptions; raised
    errors abort the agent loop with much less context. The wrapped
    ``content`` is whatever the upstream MCP tool returned —
    typically a JSON-decoded dict / list — already de-stringified
    from the MCP text-payload.
    """

    ok: bool
    tool: str
    content: Any | None = Field(default=None, description="Decoded tool output (dict/list/str).")
    error: str | None = None


def _config() -> tuple[str | None, str | None]:
    """Return ``(base_url, bearer_token)`` from env. Either may be None."""
    base = os.environ.get("CRABCC_HITL_MCP_BASE_URL")
    token = os.environ.get("CRABCC_HITL_MCP_API_TOKEN")
    return base or None, token or None


async def call_tool(  # noqa: PLR0911 — early-exit per error class is clearer than nested branches
    tool_name: str,
    arguments: dict[str, Any],
    *,
    timeout_s: float = 15.0,
) -> McpToolResult:
    """Invoke an MCP tool over HTTP/JSON-RPC.

    Args:
        tool_name: Tool identifier as registered on the MCP server
            (e.g. ``"sym"``, ``"refs"``, ``"memory.search"``).
        arguments: Tool-specific argument map. Sent verbatim under
            ``params.arguments``.
        timeout_s: Wall-clock cap on the round-trip.

    Returns:
        :class:`McpToolResult`. ``ok=False`` on transport failure /
        unavailable MCP / tool-side error. ``content`` is the
        JSON-decoded text payload when the tool returns one.
    """
    base, token = _config()
    if base is None:
        logger.debug("mcp tool %s skipped — MCP_BASE_URL unset", tool_name)
        return McpToolResult(
            ok=False,
            tool=tool_name,
            error="crabcc MCP server not configured (set CRABCC_HITL_MCP_BASE_URL to enable)",
        )
    url = f"{base.rstrip('/')}/mcp"
    headers = {"Content-Type": "application/json", "Accept": "application/json"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    body = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": tool_name, "arguments": arguments},
    }
    logger.debug("mcp call %s args=%s", tool_name, arguments)
    try:
        async with httpx.AsyncClient(timeout=timeout_s) as client:
            resp = await client.post(url, content=orjson.dumps(body), headers=headers)
    except httpx.HTTPError as e:
        logger.warning("mcp call %s transport error: %s", tool_name, e)
        return McpToolResult(ok=False, tool=tool_name, error=f"transport error: {e}")

    if resp.status_code >= 400:
        snippet = resp.text[:200]
        logger.warning("mcp call %s http %d: %s", tool_name, resp.status_code, snippet)
        return McpToolResult(
            ok=False,
            tool=tool_name,
            error=f"http {resp.status_code}: {snippet}",
        )

    # Decode the JSON-RPC envelope.
    try:
        envelope = orjson.loads(resp.content)
    except orjson.JSONDecodeError as e:
        return McpToolResult(ok=False, tool=tool_name, error=f"bad envelope: {e}")

    if "error" in envelope:
        err = envelope["error"]
        return McpToolResult(
            ok=False,
            tool=tool_name,
            error=f"mcp error {err.get('code')}: {err.get('message')}",
        )

    result = envelope.get("result", {})
    if result.get("isError"):
        # Tool-side error reported via the standard MCP shape.
        first = (result.get("content") or [{}])[0].get("text", "")
        return McpToolResult(ok=False, tool=tool_name, error=f"tool error: {first}")

    # Standard MCP success — pull text content. crabcc tools return
    # JSON strings; decode them so the agent gets typed data.
    content_blocks = result.get("content") or []
    if not content_blocks:
        return McpToolResult(ok=True, tool=tool_name, content=None)
    text = content_blocks[0].get("text")
    if text is None:
        return McpToolResult(ok=True, tool=tool_name, content=content_blocks[0])
    # Try JSON-decode the text; if it isn't JSON, return the string.
    try:
        decoded = orjson.loads(text)
        return McpToolResult(ok=True, tool=tool_name, content=decoded)
    except orjson.JSONDecodeError:
        return McpToolResult(ok=True, tool=tool_name, content=text)
