"""LangChain @tool wrappers around `crabcc` CLI lookups."""

from __future__ import annotations

import json
import os
import subprocess
from typing import Any

from langchain_core.tools import tool

CRABCC_BIN = os.environ.get("CRABCC_BIN", "crabcc")
CRABCC_ROOT = os.environ.get("CRABCC_ROOT", os.getcwd())


def _run(args: list[str]) -> Any:
    cmd = [CRABCC_BIN, *args, "--root", CRABCC_ROOT]
    proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if proc.returncode != 0:
        raise RuntimeError(
            f"crabcc exited {proc.returncode}: {proc.stderr.strip() or proc.stdout.strip()}"
        )
    return json.loads(proc.stdout)


@tool
def crabcc_sym(name: str) -> str:
    """Find where a symbol is defined in the indexed repo."""
    return json.dumps(_run(["sym", name]), indent=2)


@tool
def crabcc_refs(name: str, limit: int = 20) -> str:
    """Find references to a symbol (capped for token efficiency)."""
    return json.dumps(_run(["refs", name, "--limit", str(limit)]), indent=2)


@tool
def crabcc_callers(name: str, count_only: bool = False) -> str:
    """Find call sites of a function or method."""
    args = ["callers", name]
    if count_only:
        args.append("--count")
    return json.dumps(_run(args), indent=2)


@tool
def crabcc_outline(path: str) -> str:
    """Outline every symbol in a source file."""
    return json.dumps(_run(["outline", path]), indent=2)


CRABCC_TOOLS = [crabcc_sym, crabcc_refs, crabcc_callers, crabcc_outline]
