#!/usr/bin/env python3
"""
Second-layer benchmark: crabcc vs the C++ codeindex.cc baseline that the
README references.

This mirrors the shape of bench/raw-bench.py — same TRIALS / TIMEOUT / JSON
output schema — but swaps the "raw grep" side for the `codeindex` (a.k.a.
`cidx`) binary from https://github.com/codeindex-cc/codeindex (also mirrored
at https://github.com/peterlodri-sec/codeindex.cc). codeindex.cc is the
prior-art symbol indexer that crabcc draws its schema shape from.

Install (one of):
  brew install codeindex                  # if the homebrew tap is up
  cargo install --git https://github.com/codeindex-cc/codeindex   # rust port
  # or build from source per the upstream README

If `codeindex` is NOT on $PATH at runtime this script EXITS 0 (skip, not
fail) with a clear install hint — so CI doesn't break on machines without
the C++ baseline. Pass --baseline-fallback to fall back to a clearly-labeled
ctags + grep approximation.

Usage:
  python3 bench/codeindex-vs-crabcc.py --repo /path/to/fixture
  python3 bench/codeindex-vs-crabcc.py --repo . --baseline-fallback
  python3 bench/codeindex-vs-crabcc.py --help

Output:
  bench/results/codeindex-vs-crabcc-<UTC-timestamp>.json
"""
from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from statistics import median

TRIALS = 3
TIMEOUT = 60

# Candidate binary names for the upstream tool — tried in order.
CODEINDEX_BINARIES = ("codeindex", "cidx", "codeindex.cc")

# Tasks parallel raw-bench.py's first 5 questions — same ids so result JSON
# can be joined across both bench scripts. The `codeindex` column lists the
# shell snippet a user would plausibly run with the C++ tool. The `fallback`
# column is the ctags+grep approximation used when --baseline-fallback is
# set and codeindex is unavailable.
TASKS = [
    {
        "id": "sym-User",
        "prompt": "where is the User class defined?",
        "crabcc": "crabcc sym User",
        "codeindex": "codeindex sym User",
        "fallback": (
            r"""(test -f tags || ctags -R --languages=ruby,javascript,typescript -f tags . >/dev/null 2>&1) """
            r"""&& grep -E '^User\b' tags | awk -F'\t' '{print $2":"$3}'"""
        ),
    },
    {
        "id": "sym-Assessment",
        "prompt": "where is Assessment defined?",
        "crabcc": "crabcc sym Assessment",
        "codeindex": "codeindex sym Assessment",
        "fallback": (
            r"""(test -f tags || ctags -R --languages=ruby,javascript,typescript -f tags . >/dev/null 2>&1) """
            r"""&& grep -E '^Assessment\b' tags | awk -F'\t' '{print $2":"$3}'"""
        ),
    },
    {
        "id": "callers-count-find_by",
        "prompt": "how many call sites of find_by?",
        "crabcc": "crabcc callers find_by --count",
        "codeindex": "codeindex callers find_by --count",
        "fallback": (
            r"""grep -rEoh '\bfind_by\(' --include='*.rb' --include='*.ts' """
            r"""--include='*.tsx' --include='*.js' . | wc -l"""
        ),
    },
    {
        "id": "refs-files-Assessment",
        "prompt": "first 10 files that reference Assessment",
        "crabcc": "crabcc refs Assessment --files-only --limit 10",
        "codeindex": "codeindex refs Assessment --files-only --limit 10",
        "fallback": (
            r"""grep -rlE '\bAssessment\b' --include='*.rb' --include='*.ts' """
            r"""--include='*.tsx' --include='*.js' . | head -10"""
        ),
    },
    {
        "id": "outline-assessment-rb",
        "prompt": "outline of app/models/assessment.rb",
        "crabcc": "crabcc outline app/models/assessment.rb",
        "codeindex": "codeindex outline app/models/assessment.rb",
        "fallback": (
            r"""grep -nE '^[[:space:]]*(class|module|def|attr_(reader|writer|accessor))\b' """
            r"""app/models/assessment.rb"""
        ),
    },
]


def find_codeindex() -> str | None:
    """Return the first codeindex-flavored binary on $PATH, or None."""
    for name in CODEINDEX_BINARIES:
        p = shutil.which(name)
        if p:
            return p
    return None


def run_one(cmd: str, cwd: Path) -> tuple[int, float, int]:
    """Returns (bytes_out, wall_ms, exit_code). Negative bytes_out on timeout."""
    t0 = time.perf_counter()
    try:
        proc = subprocess.run(
            ["bash", "-c", cmd],
            cwd=str(cwd),
            capture_output=True,
            timeout=TIMEOUT,
        )
        wall_ms = (time.perf_counter() - t0) * 1000.0
        return len(proc.stdout), wall_ms, proc.returncode
    except subprocess.TimeoutExpired:
        return -1, TIMEOUT * 1000.0, 124


def trials(cmd: str, cwd: Path, n: int = TRIALS) -> dict:
    runs = [run_one(cmd, cwd) for _ in range(n)]
    bytes_out = runs[-1][0]            # last trial — disk-cache-warm
    wall_ms = min(r[1] for r in runs)  # min over warm trials
    rc = runs[-1][2]
    return {
        "bytes": bytes_out,
        "ms_min": round(wall_ms, 1),
        "ms_med": round(median(r[1] for r in runs), 1),
        "rc": rc,
        "tokens_approx": bytes_out // 4 if bytes_out >= 0 else None,
    }


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(
        prog="codeindex-vs-crabcc",
        description=(
            "Compare crabcc CLI against the C++ codeindex.cc baseline. "
            "Skips with exit 0 if codeindex is not installed unless "
            "--baseline-fallback is set."
        ),
    )
    p.add_argument(
        "--repo",
        type=Path,
        required=True,
        help="Fixture repository to benchmark inside (must already have a .crabcc index).",
    )
    p.add_argument(
        "--baseline-fallback",
        action="store_true",
        help=(
            "If codeindex isn't installed, run a ctags+grep approximation "
            "instead. Output is clearly labeled 'FALLBACK BASELINE — NOT "
            "TRUE codeindex.cc'. Numbers are not directly comparable to a "
            "real codeindex run."
        ),
    )
    p.add_argument(
        "--out",
        type=Path,
        default=None,
        help="Override output JSON path (default: bench/results/codeindex-vs-crabcc-<ts>.json).",
    )
    return p.parse_args()


def main() -> int:
    args = parse_args()
    repo: Path = args.repo.resolve()

    if not repo.is_dir():
        print(f"error: {repo} is not a directory", file=sys.stderr)
        return 2
    if not (repo / ".crabcc" / "index.db").exists():
        print(
            f"error: {repo} has no .crabcc index — run `crabcc index` first",
            file=sys.stderr,
        )
        return 2

    codeindex_bin = find_codeindex()
    using_fallback = False

    if codeindex_bin is None:
        if not args.baseline_fallback:
            print(
                "codeindex not installed; skipping codeindex-vs-crabcc bench.\n"
                "  install one of:\n"
                "    brew install codeindex\n"
                "    cargo install --git https://github.com/codeindex-cc/codeindex\n"
                "  or pass --baseline-fallback for a ctags+grep approximation.",
                file=sys.stderr,
            )
            return 0  # SKIP, not FAIL
        using_fallback = True
        print(
            "WARNING: codeindex binary not found on $PATH.\n"
            "         Running FALLBACK BASELINE (ctags + grep).\n"
            "         Numbers below are NOT a true codeindex.cc comparison.",
            file=sys.stderr,
        )
    else:
        print(f"using codeindex baseline: {codeindex_bin}", file=sys.stderr)

    results = []
    baseline_label = "fallback" if using_fallback else "codeindex"
    for t in TASKS:
        print(f"  [{t['id']}]", flush=True)
        crab = trials(t["crabcc"], repo)
        baseline_cmd = t["fallback"] if using_fallback else t["codeindex"]
        baseline = trials(baseline_cmd, repo)
        results.append({
            "id": t["id"],
            "prompt": t["prompt"],
            "crabcc": {"cmd": t["crabcc"], **crab},
            "baseline": {
                "kind": baseline_label,
                "cmd": baseline_cmd,
                **baseline,
            },
        })

    here = Path(__file__).parent
    if args.out:
        out_path = args.out
    else:
        ts = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        out_path = here / "results" / f"codeindex-vs-crabcc-{ts}.json"
    out_path.parent.mkdir(parents=True, exist_ok=True)
    payload = {
        "schema": "codeindex-vs-crabcc/v1",
        "repo": str(repo),
        "baseline_kind": baseline_label,
        "baseline_bin": codeindex_bin,
        "fallback_warning": (
            "FALLBACK BASELINE — NOT TRUE codeindex.cc"
            if using_fallback
            else None
        ),
        "trials": TRIALS,
        "timeout_s": TIMEOUT,
        "results": results,
    }
    out_path.write_text(json.dumps(payload, indent=2))
    print(f"\nwrote {out_path}")

    # Inline summary table.
    if using_fallback:
        print("\n*** FALLBACK BASELINE — NOT TRUE codeindex.cc ***")
    print()
    label = "fallback" if using_fallback else "codeidx"
    hdr = (
        f"{'task':<26} "
        f"{'crabcc B':>9} {label+' B':>11} "
        f"{'crabcc ms':>10} {label+' ms':>11}  "
        f"{'speedup':>8}"
    )
    print(hdr)
    print("-" * len(hdr))
    for r in results:
        c, b = r["crabcc"], r["baseline"]
        sp = b["ms_min"] / c["ms_min"] if c["ms_min"] > 0 else 0
        print(
            f"{r['id']:<26} "
            f"{c['bytes']:>9} {b['bytes']:>11} "
            f"{c['ms_min']:>10.1f} {b['ms_min']:>11.1f}  "
            f"{sp:>7.2f}x"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
