#!/usr/bin/env python3
"""sweep-nixery.py — bloaty + BOLT analysis matrix for the Nix-built bins.

Companion to sweep.py (which handles the Rust/crabcc matrix). This script
covers the non-Cargo targets in nix/overlays/:

    python315t-optimized   CPython 3.15b2 (PGO + ThinLTO + JIT + mimalloc + aws-lc)
    node-optimized         Node.js 26     (aws-lc + jemalloc + mold + ThinLTO + 1 GB semi-space)

Two phases, same shape as sweep.py:
  Phase A (BUILD, parallel) — nix-build each derivation, then run BOLT
                              instrumentation + workload + optimize.
  Phase B (MEASURE, serial) — bloaty section breakdown, hyperfine startup
                              latency, optional cargo-bloat on crabcc-compact-serve.

REQUIREMENTS
    nix-build, bloaty, hyperfine
    llvm-bolt + merge-fdata  (BOLT legs only)
    cargo-bloat              (optional, --cargo-bloat flag)

Stdlib only.
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass, field, asdict
from datetime import datetime, timezone
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
NIX_PKGS = REPO_ROOT / "nix" / "nixery-pkgs" / "default.nix"


# ---------------------------------------------------------------------------
# Leg definition
# ---------------------------------------------------------------------------

@dataclass
class NixLeg:
    id: str
    attr: str           # nix-build attribute (e.g. python315t-optimized)
    bin_rel: str        # path inside the store result (e.g. bin/python3)
    bolt: bool = False

    # filled during the run
    status: str = "pending"
    detail: str = ""
    build_seconds: float | None = None
    bin_path: str | None = None

    # bloaty
    file_bytes: int | None = None
    text_bytes: int | None = None
    rodata_bytes: int | None = None

    # hyperfine
    startup_mean_s: float | None = None
    startup_stddev_s: float | None = None

    # cargo-bloat (crabcc-compact-serve only)
    bloat_top: list = field(default_factory=list)


def default_matrix() -> list[NixLeg]:
    return [
        NixLeg("python-base",  "python315t-optimized", "bin/python3",  bolt=False),
        NixLeg("python-bolt",  "python315t-optimized", "bin/python3",  bolt=True),
        NixLeg("node-base",    "node-optimized",       "bin/node",     bolt=False),
        NixLeg("node-bolt",    "node-optimized",       "bin/node",     bolt=True),
    ]


# ---------------------------------------------------------------------------
# Tooling checks
# ---------------------------------------------------------------------------

def have(cmd: str) -> bool:
    return shutil.which(cmd) is not None


# ---------------------------------------------------------------------------
# Phase A — build
# ---------------------------------------------------------------------------

def nix_build(attr: str, out_link: Path, log) -> Path:
    cmd = ["nix-build", str(NIX_PKGS), "-A", attr, "--out-link", str(out_link),
           "--no-out-link" if out_link is None else ""]
    cmd = [c for c in cmd if c]  # drop empty
    cmd = ["nix-build", str(NIX_PKGS), "-A", attr, "--out-link", str(out_link)]
    log(f"  $ {' '.join(cmd)}")
    subprocess.run(cmd, check=True, stdout=log.file, stderr=subprocess.STDOUT)
    return out_link


def run_startup_workload(binary: Path, log, n: int = 5):
    """Exercise the binary lightly so BOLT sees startup-representative profile."""
    flag = "--version"
    for _ in range(n):
        subprocess.run([str(binary), flag], check=False,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def bolt_optimize(binary: Path, work: Path, log) -> Path:
    name = binary.name
    inst = work / f"{name}.inst"
    fdata_base = work / f"{name}.fdata"

    # 1. Instrument.
    subprocess.run(
        ["llvm-bolt", str(binary),
         "-instrument",
         f"-instrumentation-file={fdata_base}",
         "-instrumentation-file-append-pid",
         "-o", str(inst)],
        check=True, stdout=log.file, stderr=subprocess.STDOUT)

    # 2. Workload.
    run_startup_workload(inst, log, n=10)

    # 3. Merge per-pid drops.
    drops = list(work.glob(f"{name}.fdata*"))
    merged = work / f"{name}.merged.fdata"
    with open(merged, "w") as out:
        subprocess.run(["merge-fdata", *[str(d) for d in drops]],
                       check=True, stdout=out, stderr=log.file)

    # 4. Optimize layout.
    optimized = work / f"{name}.bolt"
    subprocess.run(
        ["llvm-bolt", str(binary),
         "-data", str(merged),
         "-o", str(optimized),
         "-reorder-blocks=ext-tsp",
         "-reorder-functions=hfsort",
         "-split-functions",
         "-split-all-cold",
         "-split-eh",
         "-icf=1",
         "-dyno-stats"],
        check=True, stdout=log.file, stderr=subprocess.STDOUT)

    stripper = shutil.which("llvm-strip") or shutil.which("strip")
    if stripper:
        subprocess.run([stripper, str(optimized)], check=False,
                       stdout=log.file, stderr=subprocess.STDOUT)
    return optimized


def build_leg(leg: NixLeg, work: Path, bolt_ok: bool) -> NixLeg:
    leg_dir = work / leg.id
    leg_dir.mkdir(parents=True, exist_ok=True)
    logf = open(leg_dir / "build.log", "w")

    class Log:
        file = logf
        def __call__(self, msg): logf.write(msg + "\n"); logf.flush()

    log = Log()
    t0 = time.time()
    try:
        if leg.bolt and not bolt_ok:
            leg.status, leg.detail = "skipped", "llvm-bolt/merge-fdata not found"
            return leg

        store_link = leg_dir / "result"
        nix_build(leg.attr, store_link, log)
        binary = store_link / leg.bin_rel

        if not binary.exists():
            raise FileNotFoundError(f"{binary} not found after nix-build")

        if leg.bolt:
            binary = bolt_optimize(binary, leg_dir, log)

        leg.bin_path = str(binary)
        leg.build_seconds = round(time.time() - t0, 1)
        leg.status = "ok"
    except Exception as e:
        leg.status = "error"
        leg.detail = str(e)
        log(f"ERROR: {e}")
    finally:
        logf.close()
    return leg


# ---------------------------------------------------------------------------
# Phase B — measure (serial)
# ---------------------------------------------------------------------------

def bloaty_breakdown(binary: Path) -> tuple[int | None, int | None, int | None]:
    """Return (.text bytes, .rodata bytes, file bytes) via bloaty."""
    if not have("bloaty"):
        return None, None, None
    try:
        out = subprocess.check_output(
            ["bloaty", "--csv", "-d", "sections", str(binary)],
            text=True, stderr=subprocess.DEVNULL)
        text = rodata = 0
        for line in out.splitlines()[1:]:  # skip header
            parts = line.split(",")
            if len(parts) < 3:
                continue
            section, _, vm_size = parts[0], parts[1], parts[2]
            try:
                sz = int(vm_size)
            except ValueError:
                continue
            if section == ".text":
                text = sz
            elif section == ".rodata":
                rodata = sz
        file_bytes = binary.stat().st_size
        return text or None, rodata or None, file_bytes
    except Exception:
        return None, None, None


def measure_leg(leg: NixLeg, work: Path, pin: str | None, runs: int):
    binary = Path(leg.bin_path)
    print(f"  measure {leg.id} → {binary}")

    leg.text_bytes, leg.rodata_bytes, leg.file_bytes = bloaty_breakdown(binary)

    # Startup latency — `--version` is the most isolated startup signal.
    pin_prefix = (["taskset", "-c", pin] if pin and have("taskset") else [])
    hj = work / f"{leg.id}.hyperfine.json"
    cmd = " ".join(pin_prefix + [str(binary), "--version"])
    try:
        subprocess.run(
            ["hyperfine", "--warmup", "3", "--runs", str(runs),
             "--export-json", str(hj), cmd],
            check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        data = json.loads(hj.read_text())["results"][0]
        leg.startup_mean_s  = round(data["mean"], 5)
        leg.startup_stddev_s = round(data.get("stddev") or 0.0, 5)
    except Exception as e:
        print(f"    hyperfine failed: {e}")


def cargo_bloat_crabcc(work: Path, runs: int = 1) -> list[dict]:
    """Run cargo bloat on crabcc-compact-serve and return top symbols."""
    if not have("cargo-bloat") and not have("cargo"):
        return []
    out_file = work / "cargo-bloat.json"
    try:
        subprocess.run(
            ["cargo", "bloat", "--release", "-p", "crabcc-compact-serve",
             "--message-format=json", "-n", "20"],
            cwd=REPO_ROOT, check=True,
            stdout=out_file.open("w"), stderr=subprocess.DEVNULL)
        raw = json.loads(out_file.read_text())
        return raw.get("functions", [])[:20]
    except Exception as e:
        print(f"  cargo-bloat failed: {e}")
        return []


# ---------------------------------------------------------------------------
# Report
# ---------------------------------------------------------------------------

def render_report(legs: list[NixLeg], cargo_bloat: list[dict], meta: dict) -> str:
    ok = [l for l in legs if l.status == "ok"]
    by_speed = sorted([l for l in ok if l.startup_mean_s is not None],
                      key=lambda l: l.startup_mean_s)
    by_size  = sorted([l for l in ok if l.file_bytes is not None],
                      key=lambda l: l.file_bytes)

    def kib(b): return f"{b/1024:.0f}" if b else "—"

    lines = [
        "# bench-nixery — binary optimization report",
        "",
        f"- generated: {meta['generated']}",
        f"- host: {meta['host']}  ·  wall: {meta['wall_seconds']}s",
        f"- legs: {len(legs)} (ok {len(ok)}, "
        f"skipped {sum(l.status=='skipped' for l in legs)}, "
        f"error {sum(l.status=='error' for l in legs)})",
        "",
        "## Startup latency (`--version`, lower is better)",
        "",
        "| leg | attr | bolt | startup mean (s) | ±stddev | build (s) |",
        "|---|---|:--:|--:|--:|--:|",
    ]
    base_node = next((l for l in by_speed if l.id == "node-base"), None)
    base_py   = next((l for l in by_speed if l.id == "python-base"), None)
    for l in by_speed:
        base = base_node if "node" in l.id else base_py
        delta = ""
        if base and base.startup_mean_s and l.startup_mean_s and base.id != l.id:
            pct = (l.startup_mean_s - base.startup_mean_s) / base.startup_mean_s * 100
            delta = f" ({pct:+.1f}%)"
        lines.append(
            f"| `{l.id}` | `{l.attr}` | {'✓' if l.bolt else ''} | "
            f"{l.startup_mean_s}{delta} | {l.startup_stddev_s} | {l.build_seconds} |")

    lines += [
        "",
        "## Binary footprint (bloaty, lower is better)",
        "",
        "| leg | file (KiB) | .text (KiB) | .rodata (KiB) |",
        "|---|--:|--:|--:|",
    ]
    for l in by_size:
        lines.append(
            f"| `{l.id}` | {kib(l.file_bytes)} | {kib(l.text_bytes)} | {kib(l.rodata_bytes)} |")

    if cargo_bloat:
        lines += [
            "",
            "## crabcc-compact-serve — top symbols by size (cargo-bloat)",
            "",
            "| symbol | size (KiB) |",
            "|---|--:|",
        ]
        for sym in cargo_bloat[:15]:
            name = sym.get("name", "?")[:80]
            size = sym.get("size", 0)
            lines.append(f"| `{name}` | {size/1024:.1f} |")

    skipped = [l for l in legs if l.status in ("skipped", "error")]
    if skipped:
        lines += ["", "## Skipped / errored", ""]
        for l in skipped:
            lines.append(f"- `{l.id}` — **{l.status}**: {l.detail}")
    lines.append("")
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main() -> int:
    ap = argparse.ArgumentParser(description="bench-nixery — bloaty+BOLT sweep for Nix bins")
    ap.add_argument("--work", default=str(REPO_ROOT / "bench" / "nixery"),
                    help="scratch dir (default: bench/nixery)")
    ap.add_argument("--out",  default=str(REPO_ROOT / "bench" / "results"),
                    help="report output dir (default: bench/results)")
    ap.add_argument("--jobs", type=int, default=0,
                    help="parallel build legs (default: min(legs, cores))")
    ap.add_argument("--runs", type=int, default=20,
                    help="hyperfine runs for startup latency (default: 20)")
    ap.add_argument("--pin",  default=None,
                    help="taskset core spec for measurements, e.g. '0-3'")
    ap.add_argument("--only", default=None,
                    help="comma list of leg ids (e.g. node-base,node-bolt)")
    ap.add_argument("--no-bolt", action="store_true",
                    help="skip BOLT legs (faster, just bloaty analysis)")
    ap.add_argument("--cargo-bloat", action="store_true",
                    help="run cargo bloat on crabcc-compact-serve")
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()

    legs = default_matrix()
    if args.no_bolt:
        legs = [l for l in legs if not l.bolt]
    if args.only:
        want = set(args.only.split(","))
        legs = [l for l in legs if l.id in want]
        if not legs:
            print(f"no legs match --only={args.only}", file=sys.stderr)
            return 2

    bolt_ok = have("llvm-bolt") and have("merge-fdata")
    cores = os.cpu_count() or 2
    jobs = args.jobs if args.jobs > 0 else min(len(legs), cores)

    print(f"== bench-nixery == legs={len(legs)} pool={jobs}")
    print(f"   bloaty={'ok' if have('bloaty') else 'MISSING'} "
          f"hyperfine={'ok' if have('hyperfine') else 'MISSING'} "
          f"bolt={'ok' if bolt_ok else 'MISSING'} "
          f"nix-build={'ok' if have('nix-build') else 'MISSING'}")

    if not have("nix-build"):
        print("ERROR: nix-build not found", file=sys.stderr)
        return 2

    if args.dry_run:
        for l in legs:
            print(f"  {l.id:20s} attr={l.attr}  bolt={int(l.bolt)}")
        return 0

    work = Path(args.work); work.mkdir(parents=True, exist_ok=True)
    out  = Path(args.out);  out.mkdir(parents=True, exist_ok=True)
    started = time.time()

    # Phase A — parallel builds.
    print(f"\n-- phase A: building {len(legs)} legs ({jobs}-wide) --")
    built: list[NixLeg] = []
    with ThreadPoolExecutor(max_workers=jobs) as pool:
        futs = {pool.submit(build_leg, l, work, bolt_ok): l for l in legs}
        for fut in as_completed(futs):
            leg = fut.result()
            built.append(leg)
            print(f"   [{leg.status:7s}] {leg.id}"
                  + (f"  build={leg.build_seconds}s" if leg.build_seconds else "")
                  + (f"  ({leg.detail})" if leg.detail else ""))

    # Phase B — serial measure.
    print(f"\n-- phase B: measuring (serial{', pinned '+args.pin if args.pin else ''}) --")
    for leg in sorted([l for l in built if l.status == "ok"], key=lambda l: l.id):
        measure_leg(leg, work, args.pin, args.runs)

    # Optional cargo-bloat.
    cargo_bloat_data: list[dict] = []
    if args.cargo_bloat:
        print("\n-- cargo-bloat (crabcc-compact-serve) --")
        cargo_bloat_data = cargo_bloat_crabcc(work)

    meta = {
        "generated": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "host": os.uname().nodename,
        "wall_seconds": round(time.time() - started, 1),
    }

    ndjson = out / "nixery.ndjson"
    with open(ndjson, "w") as f:
        for leg in built:
            f.write(json.dumps({**meta, **asdict(leg)}) + "\n")

    report = out / "nixery-REPORT.md"
    report.write_text(render_report(built, cargo_bloat_data, meta))

    print(f"\n== done in {meta['wall_seconds']}s ==")
    print(f"   NDJSON: {ndjson}")
    print(f"   report: {report}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
