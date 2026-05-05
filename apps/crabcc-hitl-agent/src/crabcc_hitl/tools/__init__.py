"""Tool registry for the HITL agent.

Phase 0 shipped :mod:`fetch_url`. Phase 1 adds the crabcc MCP-HTTP
surface (``sym`` / ``refs`` / ``callers`` / ``files`` / ``outline``
/ ``fuzzy``) plus the memory drawer (``memory.search`` /
``memory.remember`` / ``memory.list``). Tools degrade gracefully
when ``CRABCC_HITL_MCP_BASE_URL`` is unset — they return a
structured "MCP not configured" result without raising. See
``TOOLS.md`` for the full Phase 2+ brainstorm.

Conventions used by every module in this package:

- One async function per tool, named after the tool. Args are
  primitive types (``str``, ``int``, paths) so the OpenAI Agents SDK
  can derive the JSON schema directly.
- Docstring is the LLM-facing description; first line surfaces in
  tool-pickers, follow-up paragraphs describe args + return shape.
- Errors are *returned* (as a structured failure shape) rather than
  raised — agents handle returned errors gracefully; raised
  exceptions abort the loop with much less context for the model.
- No I/O side effects on disk except in the explicit ``fs_*`` family
  (Phase 2 — HITL-gated).
"""

from __future__ import annotations

from .crabcc_callers import crabcc_callers
from .crabcc_files import crabcc_files
from .crabcc_fuzzy import crabcc_fuzzy
from .crabcc_outline import crabcc_outline
from .crabcc_refs import crabcc_refs
from .crabcc_sym import crabcc_sym
from .fetch_url import fetch_url
from .memory_list import memory_list
from .memory_remember import memory_remember
from .memory_search import memory_search

__all__ = [
    "crabcc_callers",
    "crabcc_files",
    "crabcc_fuzzy",
    "crabcc_outline",
    "crabcc_refs",
    "crabcc_sym",
    "fetch_url",
    "memory_list",
    "memory_remember",
    "memory_search",
]
