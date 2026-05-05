"""Tool registry for the HITL agent.

Phase 0 only ships :mod:`fetch_url`. Phase 1 will register the full
crabcc MCP surface (sym / refs / callers / files / outline / fuzzy /
memory.*) plus utility tools — see ``TOOLS.md`` for the brainstorm
and ship order.

Conventions used by every module in this package:

- One async function per tool, named after the tool. Args are
  primitive types (``str``, ``int``, paths) so the OpenAI Agents SDK
  can derive the JSON schema directly.
- Docstring is the LLM-facing description; first line surfaces in
  tool-pickers, follow-up paragraphs describe args + return shape.
- Errors are *returned* (as a structured failure shape) rather than
  raised — agents handle returned errors gracefully; raised
  exceptions abort the loop with much less context for the model.
- No I/O side effects on disk except in the explicit `fs_*` family.
"""

from __future__ import annotations

from .fetch_url import fetch_url

__all__ = ["fetch_url"]
