"""``crabcc memory list`` — list recent notes in the drawer."""

from __future__ import annotations

from ._mcp_client import McpToolResult, call_tool


async def memory_list(limit: int = 20) -> McpToolResult:
    """List the most recent memory drawers.

    Use this when the user wants a recent-history view of what
    they've asked you to remember; ``memory_search`` is the better
    choice for finding a specific topic.

    Args:
        limit: Cap the number of records. Default 20.

    Returns:
        ``content`` is a list of ``{id, key, created_at,
        body_preview}`` records, newest first.
    """
    return await call_tool("memory.list", {"limit": limit})
