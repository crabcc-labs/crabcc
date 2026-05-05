"""``crabcc memory remember`` — persist a note in the per-repo drawer."""

from __future__ import annotations

from ._mcp_client import McpToolResult, call_tool


async def memory_remember(key: str, body: str) -> McpToolResult:
    """Persist a note that will be retrievable in future sessions.

    Use this when the user says "remember X" or when you've just
    learned a non-trivial preference / fact / decision that future
    sessions should know.

    Args:
        key: Stable identifier — pick a slug-style key (e.g.
            ``"prefs/markdown_lib"``). Re-using a key updates the
            existing drawer.
        body: Free-text note. Markdown is fine; the search backend
            indexes the raw text.

    Returns:
        ``content`` is the upserted ``{id, key}`` record.
    """
    return await call_tool("memory.remember", {"key": key, "body": body})
