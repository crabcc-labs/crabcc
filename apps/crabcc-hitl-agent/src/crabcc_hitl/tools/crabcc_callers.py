"""``crabcc callers`` — find call sites of a function."""

from __future__ import annotations

from ._mcp_client import McpToolResult, call_tool


async def crabcc_callers(name: str, count_only: bool = False) -> McpToolResult:
    """Find every callsite of a function.

    More precise than ``crabcc_refs`` because it uses the populated
    `edges` table (only ``kind='call'`` rows) — won't show type
    annotations or imports.

    Args:
        name: Function / method name (e.g. ``"Store::open"``).
        count_only: When True, return ``{count: N}`` instead of the
            full hit list. Use this first for rough scope sizing.

    Returns:
        Either ``{count: N}`` or list of ``{file, line, src_symbol}``.
    """
    args: dict[str, object] = {"name": name}
    if count_only:
        args["count"] = True
    return await call_tool("callers", args)
