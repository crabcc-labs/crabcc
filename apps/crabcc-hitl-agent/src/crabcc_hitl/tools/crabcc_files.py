"""``crabcc files`` — list indexed source files."""

from __future__ import annotations

from ._mcp_client import McpToolResult, call_tool


async def crabcc_files(
    under: str | None = None,
    ext: str | None = None,
    limit: int = 50,
) -> McpToolResult:
    """List indexed source files. Replaces ``ls -R`` / ``find`` for the agent.

    Args:
        under: Restrict to files below this directory (repo-relative
            path, e.g. ``"crates/crabcc-core/src"``). Optional.
        ext: Restrict to a single file extension without the dot
            (e.g. ``"rs"``, ``"py"``, ``"ts"``). Optional.
        limit: Cap the number of files returned. Default 50.

    Returns:
        ``content`` is a list of file paths (strings).
    """
    args: dict[str, object] = {"limit": limit}
    if under:
        args["under"] = under
    if ext:
        args["ext"] = ext
    return await call_tool("files", args)
