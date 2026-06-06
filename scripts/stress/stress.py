#!/usr/bin/env python3
"""
stress.py — parallel stress + fuzz harness for the `crabcc` CLI.

Hammers the real subcommand surface from many workers at once against a shared
index/memory DB, mixing *valid* args (real symbol names / files harvested from
the index) with *fuzzed* args (empty, huge, unicode, format-string, path
traversal, control chars, regex bombs). The point is to surface real defects —
panics, segfaults, deadlocks, hangs — as distinct from the clean non-zero exits
that bad input is *supposed* to produce.

Outputs an ndjson event log + a markdown report. Exits non-zero if any CRASH or
TIMEOUT is seen, so it can gate CI.

Stdlib only. Usage:
    scripts/stress/stress.py --workers 16 --duration 30
    scripts/stress/stress.py --iterations 2000 --writers 2 --seed 7
"""
from __future__ import annotations
import argparse, glob, json, os, random, shutil, sqlite3, statistics, subprocess, sys, time
from collections import Counter, defaultdict
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime, timezone
from pathlib import Path

# ── corpus: fuzz mutations applied to otherwise-valid args ──────────────────
FUZZ_STRINGS = [
    "", " ", "\t", "\n", "::", ":::", "..", "../", "../../etc/passwd",
    "%s%s%s%n", "{}", "{0}", "${PATH}", "$(reboot)", "`id`", "'; DROP TABLE symbols;--",
    "\\", "\\\\", "\x00truncated", "\x01\x02\x03", "—unicode—",
    "ｆｕｌｌｗｉｄｔｈ", "😀🔥💥", "Σψμβολ", "a" * 4096, "A" * 65536,
    "-" * 64, "--definitely-not-a-flag", "-x", "—", "*", "**", ".*", "(.*)+$",
    "[", "(", "//", "{{{{", "NULL", "0x0", "-1", "9" * 40,
]

def harvest(db: Path, n: int = 600):
    """Pull real names / qualified names / files / kinds from the index DB."""
    out = {"names": [], "qualified": [], "files": [], "kinds": []}
    if not db or not db.exists():
        return out
    try:
        c = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
        out["names"] = [r[0] for r in c.execute(
            "SELECT DISTINCT name FROM symbols WHERE name<>'' ORDER BY RANDOM() LIMIT ?", (n,))]
        out["qualified"] = [r[0] for r in c.execute(
            "SELECT DISTINCT qualified FROM symbols WHERE qualified IS NOT NULL ORDER BY RANDOM() LIMIT ?", (n,))]
        out["files"] = [r[0] for r in c.execute(
            "SELECT DISTINCT path FROM files ORDER BY RANDOM() LIMIT ?", (n,))]
        out["kinds"] = [r[0] for r in c.execute("SELECT DISTINCT kind FROM symbols")]
        c.close()
    except Exception as e:  # noqa: BLE001
        print(f"[warn] could not harvest from {db}: {e}", file=sys.stderr)
    return out

def find_index_db(explicit: str | None) -> Path | None:
    if explicit:
        return Path(explicit)
    cands = []
    home = os.environ.get("CRABCC_HOME", str(Path.home() / ".crabcc"))
    cands += glob.glob(os.path.join(home, "repos", "*", "index.db"))
    cands += ["./.crabcc/index.db"]
    cands = [Path(c) for c in cands if Path(c).exists()]
    return max(cands, key=lambda p: p.stat().st_mtime) if cands else None

def find_bin(explicit: str | None) -> str:
    if explicit:
        return explicit
    for c in ("target/release/crabcc", "target/debug/crabcc"):
        if Path(c).exists():
            return c
    found = shutil.which("crabcc")
    if not found:
        sys.exit("crabcc binary not found (set --bin or build it)")
    return found

# ── command generators: (subcommand argv, weight) ──────────────────────────
def make_generators(rng: random.Random, corpus: dict, fuzz_rate: float):
    names = corpus["names"] or ["Store", "open", "main"]
    quals = corpus["qualified"] or ["Store::open"]
    files = corpus["files"] or ["crates/crabcc-core/src/store.rs"]

    def pick(pool):
        # With prob fuzz_rate, emit a mutation instead of a real value.
        if rng.random() < fuzz_rate:
            base = rng.choice(pool)
            mut = rng.choice(FUZZ_STRINGS)
            return rng.choice([mut, base + mut, mut + base, base[: rng.randint(0, len(base) or 1)]])
        return rng.choice(pool)

    READ = [
        (lambda: ["lookup", "sym", pick(names)], 10),
        (lambda: ["lookup", "refs", pick(names), "--limit", str(rng.randint(0, 99))], 8),
        (lambda: ["lookup", "callers", pick(quals)] + rng.choice([[], ["--count"]]), 8),
        (lambda: ["lookup", "outline", pick(files)], 6),
        (lambda: ["lookup", "fuzzy", pick(names)], 6),
        (lambda: ["lookup", "prefix", pick(names)[: rng.randint(1, 4)] or "a"], 6),
        (lambda: ["lookup", "grep", pick(names)], 5),
        (lambda: ["lookup", "files", "--ext", rng.choice(["rs", "py", "", "..", "🔥"])], 4),
        (lambda: ["graph", "walk", pick(quals), "--depth", str(rng.randint(0, 6))], 5),
        (lambda: ["graph", rng.choice(["cycles", "orphans"])], 2),
        (lambda: ["memory", rng.choice(["search", "list", "count"]), pick(names)], 4),
        (lambda: ["info", rng.choice(["track", "services"])], 2),
        (lambda: ["read", pick(files)], 3),
    ]
    WRITE = [
        (lambda: ["index", "refresh"], 3),
        (lambda: ["graph", "build"], 1),
        (lambda: ["memory", "remember", f"stress:{rng.randint(0,10**9)}", pick(names)], 3),
    ]
    return READ, WRITE

def expand(gens):
    pool = []
    for fn, w in gens:
        pool += [fn] * w
    return pool

def classify(rc: int, stderr: str) -> str:
    se = stderr.lower()
    if rc is None:
        return "TIMEOUT"
    if rc < 0 or rc in (101, 134, 139):  # signal / Rust panic / SIGABRT / SIGSEGV
        return "CRASH"
    if "panicked at" in se or "rust_backtrace" in se or "internal error" in se:
        return "CRASH"
    if rc == 0:
        return "OK"
    return "CLEAN_ERR"

def run_one(binp: str, argv: list[str], timeout: float):
    t0 = time.perf_counter()
    try:
        p = subprocess.run([binp, *argv], capture_output=True, text=True,
                           timeout=timeout, errors="replace")
        rc, out, err = p.returncode, p.stdout, p.stderr
    except subprocess.TimeoutExpired:
        rc, out, err = None, "", "<timeout>"
    except (ValueError, OSError) as e:
        # Arg can't reach execve at all (embedded NUL, E2BIG, …) — not a crabcc
        # defect, the kernel/runtime rejects it before the process starts.
        dt = time.perf_counter() - t0
        return {"argv": argv, "rc": "n/a", "ms": round(dt * 1000, 2),
                "out_bytes": 0, "err_bytes": 0, "outcome": "UNRUNNABLE",
                "err_head": f"{type(e).__name__}: {e}"[:240]}
    dt = time.perf_counter() - t0
    return {
        "argv": argv, "rc": rc, "ms": round(dt * 1000, 2),
        "out_bytes": len(out), "err_bytes": len(err),
        "outcome": classify(rc, err),
        "err_head": err[:240] if err else "",
    }

def main():
    ap = argparse.ArgumentParser(description="parallel stress + fuzz harness for crabcc")
    ap.add_argument("--bin"); ap.add_argument("--db")
    ap.add_argument("--workers", type=int, default=min(os.cpu_count() or 4, 16))
    g = ap.add_mutually_exclusive_group()
    g.add_argument("--duration", type=float, help="seconds to run")
    g.add_argument("--iterations", type=int, help="total invocations")
    g.add_argument("--soak", type=float, metavar="SECONDS",
                   help="soak mode: sustained mix for N s with periodic resource sampling "
                        "(tracks latency drift, DB/WAL growth, fd leaks)")
    ap.add_argument("--sample-interval", type=float, default=10.0, help="soak sampling period (s)")
    ap.add_argument("--writers", type=int, default=0, help="concurrent writer workers (mutate DB)")
    ap.add_argument("--fuzz-rate", type=float, default=0.35, help="fraction of args that get mutated")
    ap.add_argument("--cmd-timeout", type=float, default=30.0, help="per-invocation timeout (s)")
    ap.add_argument("--seed", type=int, default=None)
    ap.add_argument("--out", default="bench/stress")
    args = ap.parse_args()
    if args.soak:
        args.duration = args.soak
        if args.writers == 0:
            args.writers = 2  # soak should exercise the write path to surface WAL growth/drift
    if not args.duration and not args.iterations:
        args.duration = 30.0

    rng = random.Random(args.seed)
    binp = find_bin(args.bin)
    db = find_index_db(args.db)
    corpus = harvest(db)
    read_pool = expand(make_generators(rng, corpus, args.fuzz_rate)[0])
    write_pool = expand(make_generators(rng, corpus, args.fuzz_rate)[1])

    outdir = Path(args.out); outdir.mkdir(parents=True, exist_ok=True)
    ndjson = (outdir / "stress.ndjson").open("w")
    print(f"crabcc stress: bin={binp} db={db} workers={args.workers} writers={args.writers} "
          f"seed={args.seed} fuzz={args.fuzz_rate} "
          f"{'dur='+str(args.duration)+'s' if args.duration else 'iters='+str(args.iterations)}",
          file=sys.stderr)
    print(f"corpus: {len(corpus['names'])} names, {len(corpus['files'])} files", file=sys.stderr)

    results = []
    deadline = (time.time() + args.duration) if args.duration else None
    counter = {"n": 0}
    limit = args.iterations

    def worker(is_writer: bool):
        pool = write_pool if is_writer else read_pool
        local = []
        while True:
            if deadline and time.time() >= deadline:
                break
            if limit is not None and counter["n"] >= limit:
                break
            counter["n"] += 1  # GIL-atomic enough for a monotonic progress gauge
            argv = rng.choice(pool)()
            local.append(run_one(binp, argv, args.cmd_timeout))
        return local

    # ── soak sampler: periodically snapshot resource use + latency drift ──────
    import threading
    samples = []
    stop_sampling = threading.Event()

    def fd_count() -> int:
        try:
            return sum(len(os.listdir(f"/proc/{p}/fd"))
                       for p in os.listdir("/proc") if p.isdigit()
                       and (Path(f"/proc/{p}/comm").read_text().strip() == "crabcc"
                            if Path(f"/proc/{p}/comm").exists() else False))
        except Exception:  # noqa: BLE001
            return -1

    def sampler():
        probe_name = (corpus["names"] or ["Store"])[0]
        wal = Path(str(db) + "-wal") if db else None
        while not stop_sampling.wait(args.sample_interval):
            probe = run_one(binp, ["lookup", "sym", probe_name], args.cmd_timeout)
            s = {
                "t": round(time.time() - t0, 1),
                "db_mb": round(db.stat().st_size / 1e6, 2) if db and db.exists() else None,
                "wal_mb": round(wal.stat().st_size / 1e6, 2) if wal and wal.exists() else 0.0,
                "probe_ms": probe["ms"], "probe_outcome": probe["outcome"],
                "crabcc_fds": fd_count(), "invocations": counter["n"],
            }
            samples.append(s)
            print(f"  soak t={s['t']}s db={s['db_mb']}MB wal={s['wal_mb']}MB "
                  f"probe={s['probe_ms']}ms fds={s['crabcc_fds']}", file=sys.stderr)

    # ThreadPool: each task blocks on a subprocess, so threads give real parallelism.
    total_workers = args.workers + args.writers
    t0 = time.time()
    samp_thread = None
    if args.soak:
        samp_thread = threading.Thread(target=sampler, daemon=True); samp_thread.start()
    with ThreadPoolExecutor(max_workers=total_workers) as ex:
        futs = [ex.submit(worker, i >= args.workers) for i in range(total_workers)]
        for f in as_completed(futs):
            results.extend(f.result())
    stop_sampling.set()
    if samp_thread:
        samp_thread.join(timeout=args.cmd_timeout + 2)
    wall = time.time() - t0

    for r in results:
        ndjson.write(json.dumps(r) + "\n")
    ndjson.close()
    if samples:
        with (outdir / "soak.ndjson").open("w") as sf:
            for s in samples:
                sf.write(json.dumps(s) + "\n")

    # ── report ──────────────────────────────────────────────────────────────
    outcomes = Counter(r["outcome"] for r in results)
    # Group latency by command *shape* (subcommand words only, no value args).
    TWO_LEVEL = {"lookup", "graph", "memory", "info", "index", "agent", "setup"}
    def cmd_key(argv):
        if argv and argv[0] in TWO_LEVEL and len(argv) > 1:
            return f"{argv[0]} {argv[1]}"
        return argv[0] if argv else "?"
    by_cmd = defaultdict(list)
    for r in results:
        by_cmd[cmd_key(r["argv"])].append(r["ms"])
    crashes = [r for r in results if r["outcome"] in ("CRASH", "TIMEOUT")]
    err_sigs = Counter(r["err_head"].splitlines()[0] for r in results
                       if r["outcome"] == "CLEAN_ERR" and r["err_head"])

    def pct(xs, p):
        return round(statistics.quantiles(sorted(xs), n=100)[p - 1], 1) if len(xs) > 1 else (xs[0] if xs else 0)

    lines = []
    lines.append("# crabcc stress + fuzz report\n")
    lines.append(f"- generated: {datetime.now(timezone.utc).isoformat()}")
    lines.append(f"- bin: `{binp}`  ·  db: `{db}`")
    lines.append(f"- workers: {args.workers} read + {args.writers} write  ·  seed: {args.seed}  ·  fuzz-rate: {args.fuzz_rate}")
    lines.append(f"- invocations: **{len(results)}** in {wall:.1f}s  ·  **{len(results)/wall:.0f}/s**\n")
    lines.append("## Outcomes\n")
    lines.append("| outcome | count |\n|---|--:|")
    for k in ("OK", "CLEAN_ERR", "CRASH", "TIMEOUT", "UNRUNNABLE"):
        lines.append(f"| {k} | {outcomes.get(k,0)} |")
    verdict = "🔴 BUGS FOUND" if crashes else "🟢 no crashes/timeouts"
    lines.append(f"\n**Verdict: {verdict}**\n")

    if samples:
        lines.append("## Soak — drift over time\n")
        first, last = samples[0], samples[-1]
        probes = [s["probe_ms"] for s in samples]
        third = max(1, len(samples) // 3)
        early = statistics.mean(probes[:third]); late = statistics.mean(probes[-third:])
        drift = (late - early) / early * 100 if early else 0
        wal_max = max((s["wal_mb"] or 0) for s in samples)
        db_growth = (last["db_mb"] or 0) - (first["db_mb"] or 0)
        fd_series = [s["crabcc_fds"] for s in samples if s["crabcc_fds"] >= 0]
        fd_note = (f"{fd_series[0]}→{fd_series[-1]}" if fd_series else "n/a")
        lines.append(f"- probe latency drift (`lookup sym`): **{early:.0f}ms → {late:.0f}ms "
                     f"({drift:+.0f}%)** {'⚠️' if drift > 50 else ''}")
        lines.append(f"- index.db growth: **{db_growth:+.2f} MB**  ·  WAL peak: **{wal_max:.2f} MB** "
                     f"{'⚠️ WAL not checkpointing' if wal_max > 50 else ''}")
        lines.append(f"- crabcc live fds (sampled): {fd_note} {'⚠️ possible fd leak' if len(fd_series)>1 and fd_series[-1] > fd_series[0]*3 else ''}\n")
        lines.append("| t(s) | db MB | wal MB | probe ms | live fds | invocations |\n|--:|--:|--:|--:|--:|--:|")
        for s in samples:
            lines.append(f"| {s['t']} | {s['db_mb']} | {s['wal_mb']} | {s['probe_ms']} | {s['crabcc_fds']} | {s['invocations']} |")
        lines.append("")

    if crashes:
        lines.append("## 🔴 Crashes / timeouts (repro)\n")
        seen = set()
        for r in crashes:
            sig = (r["outcome"], tuple(r["argv"][:2]), r["err_head"][:80])
            if sig in seen:
                continue
            seen.add(sig)
            lines.append(f"- **{r['outcome']}** `crabcc {' '.join(map(shquote, r['argv']))}`")
            if r["err_head"]:
                lines.append(f"  - `{r['err_head'].splitlines()[0][:160]}`")
        lines.append("")

    lines.append("## Latency by subcommand (ms)\n")
    lines.append("| subcommand | n | p50 | p95 | p99 | max |\n|---|--:|--:|--:|--:|--:|")
    for cmd in sorted(by_cmd, key=lambda k: -len(by_cmd[k])):
        xs = by_cmd[cmd]
        lines.append(f"| `{cmd}` | {len(xs)} | {pct(xs,50)} | {pct(xs,95)} | {pct(xs,99)} | {round(max(xs),1)} |")

    if err_sigs:
        lines.append("\n## Top clean-error signatures (expected for bad input)\n")
        for sig, n in err_sigs.most_common(10):
            lines.append(f"- ({n}×) `{sig[:140]}`")

    report = outdir / "stress-REPORT.md"
    report.write_text("\n".join(lines) + "\n")
    print("\n".join(lines))
    print(f"\n[wrote {report} and {outdir/'stress.ndjson'}]", file=sys.stderr)
    sys.exit(1 if crashes else 0)

def shquote(s: str) -> str:
    return s if s and all(c.isalnum() or c in "._:/-" for c in s) else "'" + s.replace("'", "'\\''") + "'"

if __name__ == "__main__":
    main()
