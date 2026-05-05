"""``crabcc fuzzy`` — Levenshtein-2 symbol search for misremembered names."""

from __future__ import annotations

from ._mcp_client import McpToolResult, call_tool


async def crabcc_fuzzy(pattern: str, limit: int = 10) -> McpToolResult:
    """Find symbols whose names are within edit-distance 2 of ``pattern``.

    Use this when you have a rough spelling — e.g. user types
    ``"strore"`` and you want to find ``"store"``.

    Args:
        pattern: Approximate identifier spelling.
        limit: Max number of fuzzy matches.

    Returns:
        ``content`` is a list of matching symbol records, sorted by
        edit-distance ascending.
    """
    return await call_tool("fuzzy", {"pattern": pattern, "limit": limit})
