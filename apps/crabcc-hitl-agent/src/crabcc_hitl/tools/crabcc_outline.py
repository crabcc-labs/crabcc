"""``crabcc outline`` — top-level structure of a single file."""

from __future__ import annotations

from ._mcp_client import McpToolResult, call_tool


async def crabcc_outline(file: str) -> McpToolResult:
    """Outline a file before reading it whole.

    Args:
        file: Repo-relative path. Must be one of the indexed source
            files — use :func:`crabcc_files` if you don't know the
            path yet.

    Returns:
        ``content`` is a list of every top-level fn / struct / impl
        / class with line ranges. Reading this first beats reading
        the entire file when you only need the shape.
    """
    return await call_tool("outline", {"file": file})
