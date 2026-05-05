"""``crabcc sym`` — find a symbol's definition."""

from __future__ import annotations

from ._mcp_client import McpToolResult, call_tool


async def crabcc_sym(name: str) -> McpToolResult:
    """Find where a symbol is defined in the indexed repo.

    Args:
        name: Symbol identifier — function / struct / class name.
            Exact match (case-sensitive). Use ``crabcc_fuzzy`` if
            you only have a rough spelling.

    Returns:
        ``content`` is a list of ``{name, kind, signature, file,
        line_start, line_end, parent}`` records — typically one,
        sometimes more when the same name is defined in multiple
        crates.
    """
    return await call_tool("sym", {"name": name})
