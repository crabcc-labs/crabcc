#!/usr/bin/env python3
"""
Issue #30 — moka cache micro-bench wrapper.

Builds and runs the in-process Rust harness `examples/cache_bench.rs`
in `crabcc-memory`, then formats a Markdown report. Three sections:

  1. PalaceRegistry: cold vs warm `open_for(cwd)` wall-time. Cold pays
     SQLite open + dir walk; warm is a moka cache hit.

  2. find_git_root memo: `find_git_root` (raw walk) vs
     `PalaceRegistry::resolve_git_root` (60s TTL memo). Numbers are
     per-call wall-time over 10 000 iterations.

  3. Embedding cache: `HashEmbedder` vs `CachedEmbedder` over a workload
     of 16 distinct bodies sampled 5 000 times. With `HashEmbedder` the
     inner cost is already cheap (~1 µs / call); the cache speedup
     scales linearly with the inner embedder's cost, so once
     `FastEmbedder` (issue #18) ships and `embed_one` becomes ONNX
     inference (~ms-scale), the same cache hits turn into 100-1000×
     wins.

Usage:

    python3 bench/cache-bench.py                 # build + run + report
    python3 bench/cache-bench.py --no-build      # reuse existing binary
    python3 bench/cache-bench.py --json-only     # raw JSON, no Markdown

Output lands in `bench/results/cache-bench-<timestamp>.json` and the
report is appended to `bench/results/cache-bench-REPORT.md`.

Exit codes:
  0  bench succeeded and emitted a report
  1  cargo build failed
  2  bench binary failed to run or returned malformed JSON
"""
from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import platform
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[1]
RESULTS_DIR = REPO_ROOT / "bench" / "results"
EXAMPLE_BIN = REPO_ROOT / "target" / "release" / "examples" / "cache_bench"


def cargo_build_example() -> None:
    """Build the cache_bench example in release mode (matches profile
    used by the existing `compress-bench.py` so the numbers comparable
    between sessions)."""
    cargo = shutil.which("cargo") or os.path.expanduser("~/.cargo/bin/cargo")
    cmd = [
        cargo, "build",
        "-p", "crabcc-memory",
        "--example", "cache_bench",
        "--release",
    ]
    print(f"[cache-bench] building: {' '.join(cmd)}", file=sys.stderr)
    completed = subprocess.run(cmd, cwd=REPO_ROOT)
    if completed.returncode != 0:
        sys.exit(1)


def run_bench() -> dict[str, Any]:
    if not EXAMPLE_BIN.exists():
        sys.exit(f"error: {EXAMPLE_BIN} not built; run without --no-build")
    print(f"[cache-bench] running: {EXAMPLE_BIN}", file=sys.stderr)
    proc = subprocess.run(
        [str(EXAMPLE_BIN)],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        print(proc.stderr, file=sys.stderr)
        sys.exit(2)
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError as e:
        print(f"error: bench output is not valid JSON: {e}", file=sys.stderr)
        print(proc.stdout, file=sys.stderr)
        sys.exit(2)


def fmt_ns(ns: int | float) -> str:
    """Pretty-print a ns count at the right scale."""
    n = float(ns)
    if n < 1_000:
        return f"{n:.0f} ns"
    if n < 1_000_000:
        return f"{n / 1_000:.2f} µs"
    if n < 1_000_000_000:
        return f"{n / 1_000_000:.2f} ms"
    return f"{n / 1_000_000_000:.3f} s"


def render_markdown(data: dict[str, Any]) -> str:
    pr = data["palace_registry"]
    gr = data["git_root_memo"]
    ec = data["embedding_cache"]
    now = dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%d %H:%M UTC")

    lines = [
        "# moka cache bench — issue #30",
        "",
        f"_Generated: {now} on {platform.node()} ({platform.machine()})._",
        "",
        "Three cache sites, all backed by `moka::sync::Cache`. "
        "Numbers are in-process micro-benches built with the workspace "
        "release profile (LTO=fat, codegen-units=1).",
        "",
        "## Summary",
        "",
        "| Site | Pre-cache | Post-cache | Speedup |",
        "|---|---|---|---|",
        f"| PalaceRegistry::open_for (cold→warm) | {fmt_ns(pr['cold_open_ns'])} | "
        f"{fmt_ns(pr['warm_open_ns_avg'])} | **{pr['speedup_x']}×** |",
        f"| find_git_root (raw→memo) | {fmt_ns(gr['raw_walk_ns_avg'])} | "
        f"{fmt_ns(gr['memoized_ns_avg'])} | **{gr['speedup_x']}×** |",
        f"| Embedder::embed_one (HashEmbedder→Cached) | {fmt_ns(ec['raw_embed_ns_avg'])} | "
        f"{fmt_ns(ec['cached_embed_ns_avg'])} | **{ec['speedup_x']}×** |",
        "",
        "## 1. PalaceRegistry::open_for",
        "",
        f"- Cold open (one shot): **{fmt_ns(pr['cold_open_ns'])}** — "
        "SQLite file create + schema migration + dir walk for `.git`.",
        f"- Warm open (avg over {pr['warm_iters']:,} calls): "
        f"**{fmt_ns(pr['warm_open_ns_avg'])}** — pure moka hit.",
        f"- Speedup: **{pr['speedup_x']}×**.",
        "",
        "Real-world impact: the MCP server gets one `cwd` arg per tool "
        "call. Without the cache every call re-opens SQLite. With it, "
        "each repo's palace is reused across the 10-min idle window.",
        "",
        "## 2. find_git_root memo (60 s TTL)",
        "",
        f"- Raw walk (avg over {gr['iters']:,} iters): "
        f"**{fmt_ns(gr['raw_walk_ns_avg'])}** — `canonicalize` + "
        "ancestor scan for `.git/`.",
        f"- Memoized (avg over {gr['iters']:,} iters): "
        f"**{fmt_ns(gr['memoized_ns_avg'])}** — moka hit, no syscalls.",
        f"- Speedup: **{gr['speedup_x']}×**.",
        "",
        "Tiny win individually, multiplies across every MCP tool call.",
        "",
        "## 3. CachedEmbedder",
        "",
        f"- HashEmbedder direct (avg over {ec['iters']:,} iters): "
        f"**{fmt_ns(ec['raw_embed_ns_avg'])}** — FNV + xorshift fill.",
        f"- CachedEmbedder over {ec['distinct_bodies']} distinct bodies "
        f"({ec['iters']:,} calls, ~{ec['iters'] // ec['distinct_bodies']}× hit ratio): "
        f"**{fmt_ns(ec['cached_embed_ns_avg'])}** — sha256 lookup + "
        "Arc clone.",
        f"- Speedup: **{ec['speedup_x']}×** today; expected 100–1000× "
        "once `FastEmbedder` (issue #18) lands and the inner cost is "
        "ONNX inference instead of a hash fill.",
        f"- Cache entries after the run: {ec['cache_entries']} (max "
        "capacity defaults to 4 096).",
        "",
        "## Out-of-scope (per issue #30)",
        "",
        "- `sym` / `refs` / `callers` query results — SQLite is already "
        "sub-ms; moka would add memory pressure for marginal wins.",
        "- FSST decoders — already `Arc<Codec>` per Store, no contention.",
        "",
    ]
    return "\n".join(lines)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[1])
    ap.add_argument("--no-build", action="store_true",
                    help="skip cargo build; reuse existing binary")
    ap.add_argument("--json-only", action="store_true",
                    help="print raw JSON to stdout, skip Markdown")
    args = ap.parse_args()

    if not args.no_build:
        cargo_build_example()

    data = run_bench()

    if args.json_only:
        print(json.dumps(data, indent=2))
        return 0

    RESULTS_DIR.mkdir(parents=True, exist_ok=True)
    ts = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    json_out = RESULTS_DIR / f"cache-bench-{ts}.json"
    md_out = RESULTS_DIR / "cache-bench-REPORT.md"
    json_out.write_text(json.dumps(data, indent=2) + "\n")
    md_out.write_text(render_markdown(data) + "\n")

    print(f"[cache-bench] wrote {json_out}", file=sys.stderr)
    print(f"[cache-bench] wrote {md_out}", file=sys.stderr)
    print(render_markdown(data))
    return 0


if __name__ == "__main__":
    sys.exit(main())
