#!/usr/bin/env python3
"""
Path B harness — cua-agent + crabcc-mcp wired through a thin tool adapter.

Headless equivalent of the Chrome-extension flow described in #107
Part B. Spawns crabcc --mcp as a child, exposes its 14 tools to the
cua-agent, runs one prompt end-to-end. Useful as:

  1. A regression check that the cua + MCP wiring still works after
     either side bumps.
  2. The harness the Chrome extension reuses (the wsbridge in the
     extension does the same subprocess.Popen + JSON-RPC dance, just
     packaged as a service worker).

Usage:
    python3 cua-with-crabcc.py --pr <URL> --root <repo-root>
    python3 cua-with-crabcc.py --task "explain Store::open"

Env knobs:
    CRABCC_BIN          — path to crabcc binary (default: PATH lookup)
    OLLAMA_HOST         — Ollama daemon URL (default: http://127.0.0.1:11434)
    OLLAMA_MODEL        — model id (default: qwen3.5:35b-a3b-coding-nvfp4)
    LOG_LEVEL           — DEBUG / INFO / WARN / ERROR (default: INFO)
"""

from __future__ import annotations

import argparse
import json
import logging
import os
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Any, Iterator

LOG = logging.getLogger("cua-crabcc")


# ---------------------------------------------------------------------------
# crabcc MCP adapter — thin JSON-RPC client over the child's stdio.
# Spec: https://spec.modelcontextprotocol.io/ (subset we need)
# ---------------------------------------------------------------------------


class CrabccMCP:
    """Spawns `crabcc --mcp` and exposes a `call(tool, args)` method.

    Lifecycle: `with CrabccMCP(...) as mcp: mcp.call(...)`.
    The child inherits the parent's cwd so `--root` resolution
    matches what the user expects.
    """

    def __init__(self, binary: str, root: Path) -> None:
        self.binary = binary
        self.root = root
        self.proc: subprocess.Popen[bytes] | None = None
        self._next_id = 1

    def __enter__(self) -> "CrabccMCP":
        self.proc = subprocess.Popen(
            [self.binary, "--root", str(self.root), "--mcp"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            bufsize=0,  # unbuffered; matters for line-delimited JSON
        )
        # MCP `initialize` handshake — required before `tools/call`.
        self._rpc(
            "initialize",
            {
                "protocolVersion": "2025-03-26",
                "capabilities": {"tools": {}},
                "clientInfo": {"name": "cua-with-crabcc", "version": "0.1.0"},
            },
        )
        self._notify("notifications/initialized", {})
        LOG.info("crabcc-mcp ready (root=%s)", self.root)
        return self

    def __exit__(self, *exc) -> None:
        if self.proc and self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.proc.kill()

    def list_tools(self) -> list[dict[str, Any]]:
        return self._rpc("tools/list", {}).get("tools", [])

    def call(self, name: str, args: dict[str, Any]) -> Any:
        """Invoke an MCP tool. Returns the parsed `content` payload."""
        rsp = self._rpc("tools/call", {"name": name, "arguments": args})
        # crabcc returns a single text content; parse if it looks like JSON.
        content = rsp.get("content", [])
        if not content:
            return None
        first = content[0]
        if first.get("type") == "text":
            text = first.get("text", "")
            try:
                return json.loads(text)
            except json.JSONDecodeError:
                return text
        return first

    # ---- private ----

    def _rpc(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        msg_id = self._next_id
        self._next_id += 1
        self._send({"jsonrpc": "2.0", "id": msg_id, "method": method, "params": params})
        for line in self._read():
            if line.get("id") == msg_id:
                if "error" in line:
                    raise RuntimeError(f"MCP error on {method}: {line['error']}")
                return line.get("result", {})
        raise RuntimeError(f"MCP: no response to {method}")

    def _notify(self, method: str, params: dict[str, Any]) -> None:
        self._send({"jsonrpc": "2.0", "method": method, "params": params})

    def _send(self, msg: dict[str, Any]) -> None:
        assert self.proc and self.proc.stdin
        self.proc.stdin.write(json.dumps(msg).encode() + b"\n")
        self.proc.stdin.flush()
        LOG.debug("→ %s", msg.get("method") or msg.get("id"))

    def _read(self) -> Iterator[dict[str, Any]]:
        assert self.proc and self.proc.stdout
        for raw in self.proc.stdout:
            if not raw.strip():
                continue
            try:
                msg = json.loads(raw)
            except json.JSONDecodeError:
                LOG.warning("non-JSON from crabcc: %r", raw[:80])
                continue
            LOG.debug("← %s", msg.get("id") or msg.get("method"))
            yield msg


# ---------------------------------------------------------------------------
# cua bridge — minimal. The real cua-agent SDK does the heavy lifting; this
# adapter exposes our `CrabccMCP.call` as one of its tools.
# ---------------------------------------------------------------------------


def build_cua_tools(mcp: CrabccMCP) -> list[dict[str, Any]]:
    """Return a tool spec list cua-agent can register.

    Each entry: {name, description, input_schema, run(args) -> result}.
    cua-agent's executor picks tools by name + description; we forward
    matching calls into `mcp.call`.
    """
    tools_def = mcp.list_tools()
    out = []
    for t in tools_def:
        name = t["name"]

        def make_runner(tool_name: str):
            def run(args: dict[str, Any]) -> Any:
                return mcp.call(tool_name, args)

            return run

        out.append(
            {
                "name": f"crabcc.{name}",
                "description": t.get("description", ""),
                "input_schema": t.get("inputSchema", {"type": "object"}),
                "run": make_runner(name),
            }
        )
    return out


def run_cua_agent(prompt: str, tools: list[dict[str, Any]]) -> str:
    """Wraps cua-agent boot + dispatch.

    Lives behind a try/except so this script remains useful as a wiring
    smoke even when cua-agent isn't installed (prints what WOULD have
    happened + the resolved tool list).
    """
    try:
        from cua_agent import Agent, OllamaBackend  # type: ignore
    except ImportError:
        LOG.warning(
            "cua-agent not installed — running in dry-run mode. "
            "Install with: pip install cua-agent cua-computer-server"
        )
        return _dry_run_summary(prompt, tools)

    backend = OllamaBackend(
        host=os.environ.get("OLLAMA_HOST", "http://127.0.0.1:11434"),
        model=os.environ.get("OLLAMA_MODEL", "qwen3.5:35b-a3b-coding-nvfp4"),
    )
    agent = Agent(backend=backend, tools=tools)
    return agent.run(prompt)


def _dry_run_summary(prompt: str, tools: list[dict[str, Any]]) -> str:
    lines = [
        "[dry-run — cua-agent not installed]",
        f"prompt: {prompt}",
        f"crabcc tools wired: {len(tools)}",
    ]
    for t in tools[:8]:
        lines.append(f"  - {t['name']}: {t['description'].splitlines()[0][:60]}")
    if len(tools) > 8:
        lines.append(f"  … +{len(tools) - 8} more")
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    ap.add_argument("--pr", help="GitHub PR URL the agent should review")
    ap.add_argument("--task", help="freeform task (overrides --pr template)")
    ap.add_argument(
        "--root",
        type=Path,
        default=Path.cwd(),
        help="repo root containing .crabcc/index.db (default: cwd)",
    )
    args = ap.parse_args()

    logging.basicConfig(
        level=os.environ.get("LOG_LEVEL", "INFO"),
        format="%(asctime)s %(levelname)-5s %(name)s: %(message)s",
    )

    crabcc_bin = os.environ.get("CRABCC_BIN") or shutil.which("crabcc")
    if not crabcc_bin:
        sys.stderr.write("crabcc binary not found on PATH; set CRABCC_BIN.\n")
        return 2
    if not (args.root / ".crabcc" / "index.db").exists():
        sys.stderr.write(
            f"no .crabcc/index.db at {args.root}; run `crabcc index` first.\n"
        )
        return 2

    if args.task:
        prompt = args.task
    elif args.pr:
        prompt = (
            f"Open {args.pr} in the browser, list each changed function, and "
            f"for each one call crabcc.callers (mode=files) to find its "
            f"fan-in. Output a Markdown table: function | file | callers."
        )
    else:
        prompt = (
            "List the indexed crates with crabcc.files, pick the most active "
            "one (most files), and call crabcc.outline on its lib.rs. "
            "Summarise its public surface in 5 bullets."
        )

    with CrabccMCP(crabcc_bin, args.root.resolve()) as mcp:
        tools = build_cua_tools(mcp)
        LOG.info("wired %d crabcc tools into cua", len(tools))
        result = run_cua_agent(prompt, tools)
        print(result)
    return 0


if __name__ == "__main__":
    sys.exit(main())
