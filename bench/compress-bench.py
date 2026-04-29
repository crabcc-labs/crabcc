#!/usr/bin/env python3
"""
First-layer compression bench — FSST off vs on.

For each measurement, runs three steps:
  1. Reset: rm -rf <repo>/.crabcc
  2. Index: time `crabcc index <repo>` (default: compress feature on)
  3. Train + rebuild: `crabcc compress && crabcc compress --rebuild`
  4. Stats:           `crabcc compress --stats --json`

Compare against:
  - Same flow but with `--compress=false` (or --no-default-features build)

Records to bench/results/compress-<timestamp>.json:
  - db_size_bytes (off / on)
  - index_wall_time_ms (off / on)
  - rebuild_wall_time_ms
  - stats blob (from --stats --json)
  - decode_p50/p95/p99 (from a Rust subprocess or a sample of `crabcc sym` calls)
  - throughput MB/s for bulk decode (a synthetic loop)

Usage:
  python3 bench/compress-bench.py --repo /path/to/fixture --crabcc /path/to/crabcc

NOTE: The mc-mothership fixture is large (~23K files); set --trials 1 for
smoke runs, --trials 3 for canonical numbers. Output JSON files land in
bench/results/ which is gitignored except REPORT.md / *.png / fsst-gate.md.
"""
from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import platform
import random
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

THIS_DIR = Path(__file__).resolve().parent
RESULTS_DIR = THIS_DIR / "results"
SAMPLE_DECODE_CALLS = 1000          # `crabcc sym` probes for end-to-end p99
BULK_DECODE_TARGET_ROWS = 5000      # synthetic bulk loop target


def find_default_crabcc() -> str:
    here = Path(__file__).resolve().parents[1]
    candidate = here / "target" / "release" / "crabcc"
    if candidate.exists() and os.access(candidate, os.X_OK):
        return str(candidate)
    found = shutil.which("crabcc")
    if found:
        return found
    sys.exit("error: could not find crabcc binary; pass --crabcc explicitly")


def reset_index(repo: Path) -> None:
    """Wipe `<repo>/.crabcc`. mc-mothership is read-only otherwise — only this dir is touched."""
    cc = repo / ".crabcc"
    if cc.exists():
        shutil.rmtree(cc)


def run_timed(cmd: list[str], cwd: Path | None = None,
              timeout: int = 1200) -> tuple[float, subprocess.CompletedProcess]:
    """Wall-clock a subprocess. Returns (ms, completed_process)."""
    t0 = time.perf_counter()
    proc = subprocess.run(cmd, cwd=str(cwd) if cwd else None,
                          capture_output=True, timeout=timeout)
    elapsed_ms = (time.perf_counter() - t0) * 1000.0
    return elapsed_ms, proc


def best_of(runs: list[float]) -> float:
    return min(runs) if runs else 0.0


def db_size(repo: Path) -> int:
    db = repo / ".crabcc" / "index.db"
    return db.stat().st_size if db.exists() else 0


def measure_index(crabcc: str, repo: Path, trials: int,
                  compress_flag: bool) -> tuple[float, int]:
    """
    Reset + run `crabcc index` <trials> times. Returns (best_ms, db_size_after_last_run).
    `compress_flag=False` translates to `--compress=false` on the CLI; the
    schema column still exists, rows simply land plaintext.
    """
    runs = []
    last_db = 0
    for _ in range(trials):
        reset_index(repo)
        cmd = [crabcc, "--root", str(repo)]
        if not compress_flag:
            cmd += ["--compress=false"]
        cmd += ["index"]
        ms, proc = run_timed(cmd, timeout=1800)
        if proc.returncode != 0:
            sys.stderr.write(proc.stderr.decode("utf-8", errors="replace"))
            raise RuntimeError(f"crabcc index failed (rc={proc.returncode})")
        runs.append(ms)
        last_db = db_size(repo)
    return best_of(runs), last_db


def measure_rebuild(crabcc: str, repo: Path) -> tuple[float, dict[str, Any]]:
    """`crabcc compress` (train) followed by `--rebuild`, then `--stats --json`."""
    train_cmd = [crabcc, "--root", str(repo), "compress"]
    train_ms, proc = run_timed(train_cmd, timeout=600)
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr.decode("utf-8", errors="replace"))
        return 0.0, {"error": "train failed", "rc": proc.returncode,
                     "stderr": proc.stderr.decode("utf-8", errors="replace")}

    rebuild_cmd = [crabcc, "--root", str(repo), "compress", "--rebuild"]
    rebuild_ms, proc = run_timed(rebuild_cmd, timeout=1800)
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr.decode("utf-8", errors="replace"))
        return rebuild_ms, {"error": "rebuild failed", "rc": proc.returncode}

    stats_cmd = [crabcc, "--root", str(repo), "compress", "--stats", "--json"]
    _, proc = run_timed(stats_cmd, timeout=120)
    try:
        # crabcc emits a banner on stderr + JSON on stdout. Parse stdout only.
        stats = json.loads(proc.stdout.decode("utf-8").strip())
    except Exception as e:
        stats = {"error": f"stats-json parse failed: {e}",
                 "stdout": proc.stdout.decode("utf-8", errors="replace")[:2000]}
    return train_ms + rebuild_ms, stats


def sample_symbol_names(crabcc: str, repo: Path, n: int) -> list[str]:
    """Pull n random symbol names from the index via `crabcc files`-adjacent tools.

    We can't easily query symbols list directly without an SQL helper, so use
    the SQLite DB via Python's sqlite3 — read-only — to grab a random sample.
    """
    import sqlite3
    db = repo / ".crabcc" / "index.db"
    if not db.exists():
        return []
    conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    try:
        rows = conn.execute(
            "SELECT name FROM symbols WHERE name IS NOT NULL "
            "AND length(name) > 1 ORDER BY RANDOM() LIMIT ?", (n,)
        ).fetchall()
    finally:
        conn.close()
    return [r[0] for r in rows]


def measure_decode_latency(crabcc: str, repo: Path, n_calls: int) -> dict[str, float]:
    """End-to-end probe: `crabcc sym <name>` n_calls times, record per-call ms."""
    names = sample_symbol_names(crabcc, repo, n_calls)
    if not names:
        return {"p50_ms": 0.0, "p95_ms": 0.0, "p99_ms": 0.0,
                "samples": 0, "note": "no symbol names available"}
    # Shuffle to defeat any name-order locality.
    random.shuffle(names)
    durations_ms: list[float] = []
    for name in names:
        cmd = [crabcc, "--root", str(repo), "sym", name]
        t0 = time.perf_counter()
        proc = subprocess.run(cmd, capture_output=True, timeout=10)
        durations_ms.append((time.perf_counter() - t0) * 1000.0)
        # We don't fail the bench on rc!=0 (some random names may be filter-only);
        # the wall time still reflects per-row decompress + lookup overhead.
        _ = proc
    durations_ms.sort()

    def pct(p: float) -> float:
        if not durations_ms:
            return 0.0
        i = max(0, min(len(durations_ms) - 1, int(round(p * (len(durations_ms) - 1)))))
        return durations_ms[i]

    return {
        "p50_ms": round(pct(0.50), 4),
        "p95_ms": round(pct(0.95), 4),
        "p99_ms": round(pct(0.99), 4),
        "min_ms": round(durations_ms[0], 4),
        "max_ms": round(durations_ms[-1], 4),
        "mean_ms": round(statistics.mean(durations_ms), 4),
        "samples": len(durations_ms),
    }


def measure_bulk_decode_throughput(repo: Path, target_rows: int) -> dict[str, Any]:
    """
    Synthetic bulk-decode loop in-process: SELECT every signature row from the
    SQLite DB. The signatures come back as plain UTF-8 (the query path inside
    SQLite doesn't decompress — that's done in Rust by `Store`). For an
    apples-to-apples MB/s we measure raw bytes-per-second of the SELECT loop;
    the `signature_enc=1` indicator tells us if we're seeing FSST or plaintext.

    We don't decode FSST in Python (no fsst bindings), so this number is an
    upper bound on the SQL layer; the real per-row decode latency is captured
    by `measure_decode_latency`.
    """
    import sqlite3
    db = repo / ".crabcc" / "index.db"
    if not db.exists():
        return {"error": "no index.db"}
    conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    try:
        total_rows = conn.execute("SELECT COUNT(*) FROM symbols").fetchone()[0]
        encoded_rows = conn.execute(
            "SELECT COUNT(*) FROM symbols WHERE signature_enc = 1"
        ).fetchone()[0]
        limit = min(target_rows, total_rows or 0)
        t0 = time.perf_counter()
        cur = conn.execute(
            "SELECT signature FROM symbols WHERE signature IS NOT NULL LIMIT ?",
            (limit,),
        )
        total_bytes = 0
        rows = 0
        for (blob,) in cur:
            if isinstance(blob, (bytes, bytearray)):
                total_bytes += len(blob)
            elif isinstance(blob, str):
                total_bytes += len(blob.encode("utf-8"))
            rows += 1
        elapsed = max(time.perf_counter() - t0, 1e-9)
    finally:
        conn.close()

    mb_s = (total_bytes / 1_000_000.0) / elapsed
    return {
        "rows_read": rows,
        "bytes_read": total_bytes,
        "elapsed_s": round(elapsed, 6),
        "mb_per_s": round(mb_s, 2),
        "encoded_rows_in_db": encoded_rows,
        "total_rows_in_db": total_rows,
    }


def gate_verdict(off: dict[str, Any], on: dict[str, Any]) -> dict[str, Any]:
    """Compute release-gate booleans per docs/RESEARCH-fsst.md §6.2."""
    off_size = max(off.get("db_size_bytes", 0), 1)
    on_size = max(on.get("db_size_bytes", 1), 1)
    ratio = round(off_size / on_size, 2)
    p99 = on.get("decode_p99_ms", float("inf"))
    p99_under_1ms = isinstance(p99, (int, float)) and p99 < 1.0
    off_idx = max(off.get("index_ms", 0), 1.0)
    on_idx = on.get("index_ms", 0.0)
    regression_pct = round(((on_idx - off_idx) / off_idx) * 100.0, 2)
    regression_under_10 = regression_pct < 10.0
    ratio_pass = ratio >= 1.4
    pass_all = bool(p99_under_1ms and ratio_pass and regression_under_10)
    return {
        "ratio": ratio,
        "p99_decode_under_1ms": bool(p99_under_1ms),
        "p99_decode_ms": p99,
        "indexing_regression_pct": regression_pct,
        "size_reduction_ge_1_4x": bool(ratio_pass),
        "indexing_regression_under_10pct": bool(regression_under_10),
        "release_gate_pass": pass_all,
    }


def write_gate_md(out_md: Path, summary: dict[str, Any]) -> None:
    """Render bench/results/fsst-gate.md scorecard with real numbers."""
    on = summary["fsst_on"]
    off = summary["fsst_off"]
    v = summary["verdict"]
    repo = summary["repo"]
    ts = summary.get("timestamp", "")
    host = summary["host"]
    p99_ms = v.get("p99_decode_ms", "n/a")
    p99_disp = f"{p99_ms:.3f} ms" if isinstance(p99_ms, (int, float)) else str(p99_ms)
    ratio = v.get("ratio", 0.0)
    reg = v.get("indexing_regression_pct", 0.0)

    def pf(b: bool | None) -> str:
        if b is True:
            return "PASS"
        if b is False:
            return "FAIL"
        return "n/a"

    rows = [
        ("p99 single-row decode", "<1 ms", p99_disp, pf(v.get("p99_decode_under_1ms"))),
        ("DB size reduction (signatures)", ">=1.4x", f"{ratio:.2f}x",
         pf(v.get("size_reduction_ge_1_4x"))),
        ("Indexing throughput regression", "<10%", f"{reg:+.1f}%",
         pf(v.get("indexing_regression_under_10pct"))),
        ("Test suite", "zero regressions",
         "n/a (run separately, see CI artifact)", "n/a"),
    ]

    lines = [
        f"# FSST v2.0.0-alpha release gate",
        "",
        f"Bench data: see `bench/results/compress-{ts}.json` (gitignored).",
        f"Run on: {ts}, host {host['os']}/{host['arch']}, fixture `{repo}`.",
        "",
        "| Criterion | Threshold | Measured | Pass? |",
        "|---|---|---|---|",
    ]
    for name, thr, meas, ok in rows:
        lines.append(f"| {name} | {thr} | {meas} | {ok} |")
    lines += [
        "",
        "## Raw numbers",
        "",
        f"- FSST off: index {off.get('index_ms', 0):.0f} ms, db {off.get('db_size_bytes', 0):,} B",
        f"- FSST on:  index {on.get('index_ms', 0):.0f} ms, db {on.get('db_size_bytes', 0):,} B, rebuild {on.get('rebuild_ms', 0):.0f} ms",
        f"- Bulk SQL throughput: {on.get('bulk_decode', {}).get('mb_per_s', 0)} MB/s "
        f"({on.get('bulk_decode', {}).get('rows_read', 0)} rows)",
        "",
        "## Decision",
        "",
        f"{'PASS' if v['release_gate_pass'] else 'INSPECT'} - "
        + ("recommend cutting v2.0.0-alpha.1." if v["release_gate_pass"]
           else "see failing rows above; do not cut tag yet."),
    ]
    out_md.write_text("\n".join(lines) + "\n")


def get_crabcc_version(crabcc: str) -> str:
    try:
        out = subprocess.run([crabcc, "--version"], capture_output=True, timeout=10)
        return out.stdout.decode("utf-8").strip() or "unknown"
    except Exception:
        return "unknown"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--repo", required=True, type=Path,
                    help="repo to index (read-only; we only touch <repo>/.crabcc)")
    ap.add_argument("--crabcc", default=None,
                    help="path to crabcc binary (default: target/release/crabcc, then PATH)")
    ap.add_argument("--out", default=None,
                    help="output JSON path (default: bench/results/compress-<ts>.json)")
    ap.add_argument("--trials", type=int, default=3,
                    help="number of timing trials (best-of); default 3")
    ap.add_argument("--no-fsst-binary", action="store_true",
                    help="use --compress=false runtime flag instead of rebuilding crabcc")
    ap.add_argument("--decode-calls", type=int, default=SAMPLE_DECODE_CALLS,
                    help=f"`crabcc sym` calls for decode-latency probe (default {SAMPLE_DECODE_CALLS})")
    args = ap.parse_args()

    repo = args.repo.resolve()
    if not repo.is_dir():
        sys.exit(f"error: {repo} is not a directory")

    crabcc = args.crabcc or find_default_crabcc()
    if not Path(crabcc).exists():
        sys.exit(f"error: crabcc binary not found at {crabcc}")

    timestamp = dt.datetime.utcnow().strftime("%Y%m%dT%H%M%SZ")
    RESULTS_DIR.mkdir(parents=True, exist_ok=True)
    out_path = Path(args.out) if args.out else RESULTS_DIR / f"compress-{timestamp}.json"

    print(f"# crabcc:    {crabcc}")
    print(f"# repo:      {repo}")
    print(f"# trials:    {args.trials}")
    print(f"# out:       {out_path}")

    # ------------------------------------------------------------------
    # Phase 1 — FSST OFF: index with --compress=false, capture wall + size.
    # ------------------------------------------------------------------
    print("\n[1/3] Index with FSST OFF (--compress=false)…")
    off_idx_ms, off_db = measure_index(crabcc, repo, args.trials, compress_flag=False)
    print(f"  index_ms={off_idx_ms:.0f}  db={off_db:,} bytes")

    fsst_off = {
        "index_ms": round(off_idx_ms, 2),
        "db_size_bytes": off_db,
    }

    # ------------------------------------------------------------------
    # Phase 2 — FSST ON: index normally, then train + rebuild, then stats.
    # ------------------------------------------------------------------
    print("\n[2/3] Index with FSST ON (default)…")
    on_idx_ms, on_db_pre_rebuild = measure_index(crabcc, repo, args.trials, compress_flag=True)
    print(f"  index_ms={on_idx_ms:.0f}  db_pre_rebuild={on_db_pre_rebuild:,} bytes")

    print("\n[2b/3] Train + rebuild…")
    rebuild_ms, stats = measure_rebuild(crabcc, repo)
    on_db_post = db_size(repo)
    print(f"  rebuild_ms={rebuild_ms:.0f}  db_post_rebuild={on_db_post:,} bytes")

    # ------------------------------------------------------------------
    # Phase 3 — decode probes: real-world (`crabcc sym` x N) + bulk SQL.
    # ------------------------------------------------------------------
    print(f"\n[3/3] Decode-latency probe ({args.decode_calls} `crabcc sym` calls)…")
    decode = measure_decode_latency(crabcc, repo, args.decode_calls)
    print(f"  p50={decode.get('p50_ms')}ms  p95={decode.get('p95_ms')}ms  p99={decode.get('p99_ms')}ms")

    print("\n[3b/3] Bulk SQL throughput probe…")
    bulk = measure_bulk_decode_throughput(repo, BULK_DECODE_TARGET_ROWS)
    print(f"  {bulk}")

    fsst_on = {
        "index_ms": round(on_idx_ms, 2),
        "db_size_bytes": on_db_post,
        "db_size_bytes_pre_rebuild": on_db_pre_rebuild,
        "rebuild_ms": round(rebuild_ms, 2),
        "stats": stats,
        "decode_p50_ms": decode.get("p50_ms"),
        "decode_p95_ms": decode.get("p95_ms"),
        "decode_p99_ms": decode.get("p99_ms"),
        "decode_full": decode,
        "bulk_decode": bulk,
    }

    summary = {
        "repo": str(repo),
        "timestamp": timestamp,
        "crabcc_version": get_crabcc_version(crabcc),
        "host": {"os": platform.system().lower(),
                 "arch": platform.machine(),
                 "python": platform.python_version()},
        "trials": args.trials,
        "fsst_off": fsst_off,
        "fsst_on": fsst_on,
        "verdict": gate_verdict(fsst_off, fsst_on),
    }

    out_path.write_text(json.dumps(summary, indent=2))
    print(f"\nwrote {out_path}")

    gate_md = RESULTS_DIR / "fsst-gate.md"
    write_gate_md(gate_md, summary)
    print(f"wrote {gate_md}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
