"""``crabcc refs`` — find every reference to an identifier."""

from __future__ import annotations

from ._mcp_client import McpToolResult, call_tool


async def crabcc_refs(
    name: str,
    files_only: bool = False,
    limit: int = 50,
) -> McpToolResult:
    """Find every reference (call site, type-annotation, import) of an identifier.

    Args:
        name: Identifier to search for. Exact match.
        files_only: When True, return the deduped file list instead
            of every hit. Saves ~99% of tokens on hot names.
        limit: Cap the number of hits / files returned.

    Returns:
        When ``files_only=False``: list of ``{file, line, snippet}``.
        When ``files_only=True``: list of file paths.
    """
    args: dict[str, object] = {"name": name, "limit": limit}
    if files_only:
        args["files_only"] = True
    return await call_tool("refs", args)
