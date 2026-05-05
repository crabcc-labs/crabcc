"""``crabcc memory search`` — hybrid lexical+vector search of saved notes."""

from __future__ import annotations

from ._mcp_client import McpToolResult, call_tool


async def memory_search(
    query: str,
    mode: str = "hybrid",
    limit: int = 5,
) -> McpToolResult:
    """Search the per-repo memory drawer for relevant notes.

    Backed by FTS5 BM25 ⊕ cosine KNN fused via Reciprocal Rank
    Fusion (k = 60). Use this before answering "did we discuss X
    before?" — saves the user explaining context the agent has
    already seen.

    Args:
        query: Free-text query.
        mode: ``"lexical"`` (BM25 only), ``"vector"`` (cosine only),
            or ``"hybrid"`` (default — RRF-fused).
        limit: Top-K results.

    Returns:
        ``content`` is a list of drawers with ``{id, body, score}``.
    """
    return await call_tool(
        "memory.search",
        {"query": query, "mode": mode, "limit": limit},
    )
