#!/usr/bin/env python3
"""bench-opt-bin — advanced binary-optimization sweep harness.

Sweeps the high-effort optimization axes that sit *past* the shipped
release baseline (opt-level=3 / lto=fat / codegen-units=1 / panic=abort /
strip):

    target-cpu × allocator × PGO{off,on} × BOLT{off,on}   (+ opt-level=z corner)

Each leg builds `crabcc` on top of the reserved `release-nightly` profile
(see workspace Cargo.toml — that profile exists precisely for PGO/BOLT/opt-z
experiments), drives a realistic heavy workload (`crabcc index` over a
fixture repo) for telemetry, and measures runtime (hyperfine) + footprint
(`size -A`, file size, optional cargo-bloat). One NDJSON row per leg plus a
rendered Markdown report.

METHODOLOGY
-----------
Two phases, deliberately:
  * Phase A — BUILD (parallel). The time-dominant part. Each leg compiles in
    its own CARGO_TARGET_DIR so differing RUSTFLAGS never thrash a shared
    cache. PGO (instrument → workload → profdata merge → use-build) and BOLT
    (emit-relocs build → instrument → workload → optimize) happen here.
  * Phase B — MEASURE (serial). Timing is poisoned by a busy box, so the
    binaries are measured one at a time, optionally pinned with `taskset`.

This shape is what makes "deep matrix in one hour" real: builds are the
expensive part and they fan out across cores; measurements stay trustworthy
because they don't.

REQUIREMENTS (PATH)
-------------------
  cargo, rustc, hyperfine, size (binutils), llvm-profdata
  (`rustup component add llvm-tools-preview`). BOLT legs additionally need
  `llvm-bolt` + `merge-fdata`. `cargo bloat` is optional (--bloat).

Stdlib only.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import re
import shutil
import tarfile
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass, field, asdict
from datetime import datetime, timezone
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
PROFILE = "release-nightly"
PKG = "crabcc-cli"
BIN = "crabcc"

# Allocator name → extra cargo features. `system` means the std allocator
# (no global_allocator override); the binary crate already wires
# `mimalloc` / `jemalloc` features to #[global_allocator] cfg gates.
ALLOC_FEATURES = {
    "system": [],
    "mimalloc": ["mimalloc"],
    "jemalloc": ["jemalloc"],
}


@dataclass
class Leg:
    """One point in the optimization matrix."""

    id: str
    target_cpu: str = "x86-64-v3"
    alloc: str = "system"
    opt_level: str | None = None  # None → profile default (3); "z" → size
    pgo: bool = False
    bolt: bool = False

    # Filled in as the leg runs.
    status: str = "pending"          # pending|ok|skipped|error
    detail: str = ""
    build_seconds: float | None = None
    bin_path: str | None = None
    file_bytes: int | None = None
    text_bytes: int | None = None
    rodata_bytes: int | None = None
    total_size_bytes: int | None = None
    index_mean_s: float | None = None
    index_stddev_s: float | None = None
    query_mean_s: float | None = None
    bloat_top: list = field(default_factory=list)


def default_matrix() -> list[Leg]:
    """The fractional sweep — ~11 high-signal legs, sized for ~1h on ~32c."""
    legs: list[Leg] = []
    # Reference corners.
    legs.append(Leg("baseline-v3-system", "x86-64-v3", "system"))
    legs.append(Leg("native-system", "native", "system"))
    # Core fractional block: v3 × {mimalloc,jemalloc} × pgo × bolt.
    for alloc in ("mimalloc", "jemalloc"):
        for pgo in (False, True):
            for bolt in (False, True):
                suffix = f"{alloc}{'-pgo' if pgo else ''}{'-bolt' if bolt else ''}"
                legs.append(Leg(f"v3-{suffix}", "x86-64-v3", alloc, pgo=pgo, bolt=bolt))
    # Footprint corner.
    legs.append(Leg("v3-system-optz", "x86-64-v3", "system", opt_level="z"))
    return legs


def deep_matrix() -> list[Leg]:
    """A wider matrix (~28 legs) for big boxes — enough independent legs to
    keep every core busy *through* the single-threaded fat-LTO link phase,
    where the fractional matrix (11 legs) would leave a 32-vCPU box idle.

    Adds a full target-cpu axis (v2/v3/v4/native) and spreads PGO/BOLT across
    the strongest cpu×alloc corners."""
    cpus = ["x86-64-v2", "x86-64-v3", "x86-64-v4", "native"]
    allocs = ["system", "mimalloc", "jemalloc"]
    legs: list[Leg] = []
    # 1. Plain cross of cpu × alloc (12).
    for cpu in cpus:
        for alloc in allocs:
            legs.append(Leg(f"{cpu}-{alloc}", cpu, alloc))
    # 2. PGO / BOLT / PGO+BOLT on the front-runner corners (v3,native × system,mimalloc) → 12.
    for cpu in ("x86-64-v3", "native"):
        for alloc in ("system", "mimalloc"):
            for pgo, bolt in ((True, False), (False, True), (True, True)):
                sid = f"{cpu}-{alloc}{'-pgo' if pgo else ''}{'-bolt' if bolt else ''}"
                legs.append(Leg(sid, cpu, alloc, pgo=pgo, bolt=bolt))
    # 3. Footprint corners (4).
    for cpu in ("x86-64-v3", "x86-64-v4"):
        legs.append(Leg(f"{cpu}-system-optz", cpu, "system", opt_level="z"))
    legs.append(Leg("v3-system-optz-jemalloc", "x86-64-v3", "jemalloc", opt_level="z"))
    legs.append(Leg("native-system-optz", "native", "system", opt_level="z"))
    return legs


# ----------------------------------------------------------------------------
# Tooling preflight
# ----------------------------------------------------------------------------

def have(tool: str) -> bool:
    return shutil.which(tool) is not None


def llvm_profdata() -> str | None:
    """Find llvm-profdata (PATH, or the rustup llvm-tools sysroot)."""
    if have("llvm-profdata"):
        return "llvm-profdata"
    try:
        sysroot = subprocess.check_output(
            ["rustc", "--print", "sysroot"], text=True
        ).strip()
    except Exception:
        return None
    for p in Path(sysroot).glob("lib/rustlib/*/bin/llvm-profdata"):
        return str(p)
    return None


# ----------------------------------------------------------------------------
# Build phase
# ----------------------------------------------------------------------------

def rustflags(leg: Leg, *, profile_generate: str | None = None,
              profile_use: str | None = None) -> str:
    flags = [f"-C target-cpu={leg.target_cpu}"]
    if leg.opt_level:
        flags.append(f"-C opt-level={leg.opt_level}")
    if leg.bolt:
        # BOLT needs relocations preserved + frame pointers for accurate CFG,
        # AND an unstripped symbol table to read the function inventory.
        # `release-nightly` inherits `[profile.release] strip = true`, so the
        # override is mandatory here — without it BOLT can't process the input.
        # The optimized output is re-stripped in bolt_optimize() so the
        # footprint axis stays comparable to the (stripped) non-BOLT legs.
        flags.append("-C link-args=-Wl,--emit-relocs")
        flags.append("-C force-frame-pointers=yes")
        flags.append("-C strip=none")
    if profile_generate:
        flags.append(f"-C profile-generate={profile_generate}")
    if profile_use:
        flags.append(f"-C profile-use={profile_use}")
        flags.append("-C llvm-args=-pgo-warn-missing-function")
    return " ".join(flags)


def cargo_build(leg: Leg, target_dir: Path, env_extra: dict, jobs: int,
                log) -> Path:
    feats = ALLOC_FEATURES[leg.alloc]
    cmd = [
        "cargo", "build", "--profile", PROFILE,
        "-p", PKG, "--bin", BIN, "--locked",
    ]
    if feats:
        cmd += ["--features", ",".join(feats)]
    env = {**os.environ, **env_extra,
           "CARGO_TARGET_DIR": str(target_dir),
           "CARGO_BUILD_JOBS": str(jobs)}
    # Per-leg target dirs isolate differing RUSTFLAGS but otherwise force a
    # full dependency recompile per leg (~N× redundant work). sccache shares
    # cached crate artifacts across legs whose flags match (the bulk of the
    # tree differs only in target-cpu / allocator features), turning that
    # redundancy back into useful throughput. PGO instrument/use legs embed a
    # profile path so they simply miss the cache — no harm.
    if shutil.which("sccache") and "RUSTC_WRAPPER" not in env:
        env["RUSTC_WRAPPER"] = "sccache"
    log(f"  $ RUSTFLAGS='{env_extra.get('RUSTFLAGS', '')}' {' '.join(cmd)}")
    subprocess.run(cmd, cwd=REPO_ROOT, env=env, check=True,
                   stdout=log.file, stderr=subprocess.STDOUT)
    return target_dir / PROFILE / BIN


def run_workload(binary: Path, scratch: Path, log, iterations: int = 3):
    """Drive the heavy path so PGO/BOLT see realistic telemetry."""
    root = scratch / "workload-root"
    if root.exists():
        shutil.rmtree(root)
    shutil.copytree(REPO_ROOT, root, ignore=shutil.ignore_patterns(
        ".git", "target", "bench", "node_modules", ".crabcc", "dist"))
    for _ in range(iterations):
        shutil.rmtree(root / ".crabcc", ignore_errors=True)
        subprocess.run([str(binary), "index", "--root", str(root)],
                       check=False, stdout=log.file, stderr=subprocess.STDOUT)
        subprocess.run([str(binary), "sym", "Store", "--root", str(root)],
                       check=False, stdout=log.file, stderr=subprocess.STDOUT)


def build_leg(leg: Leg, work: Path, jobs: int, profdata_tool: str | None,
              bolt_ok: bool) -> Leg:
    leg_dir = work / leg.id
    leg_dir.mkdir(parents=True, exist_ok=True)
    target_dir = leg_dir / "target"
    logf = open(leg_dir / "build.log", "w")

    class Log:
        file = logf
        def __call__(self, msg): logf.write(msg + "\n"); logf.flush()

    log = Log()
    t0 = time.time()
    try:
        if leg.pgo and not profdata_tool:
            leg.status, leg.detail = "skipped", "llvm-profdata not found"
            return leg
        if leg.bolt and not bolt_ok:
            leg.status, leg.detail = "skipped", "llvm-bolt/merge-fdata not found"
            return leg

        if leg.pgo:
            # 1. Instrument build.
            pgo_raw = leg_dir / "pgo-raw"
            cargo_build(leg, target_dir,
                        {"RUSTFLAGS": rustflags(leg, profile_generate=str(pgo_raw))},
                        jobs, log)
            inst_bin = target_dir / PROFILE / BIN
            # 2. Workload → raw profiles.
            run_workload(inst_bin, leg_dir, log)
            # 3. Merge.
            merged = leg_dir / "merged.profdata"
            subprocess.run([profdata_tool, "merge", "-o", str(merged), str(pgo_raw)],
                           check=True, stdout=logf, stderr=subprocess.STDOUT)
            # 4. Use build (fresh target dir so instrumentation is gone).
            target_dir2 = leg_dir / "target-use"
            binary = cargo_build(leg, target_dir2,
                                 {"RUSTFLAGS": rustflags(leg, profile_use=str(merged))},
                                 jobs, log)
        else:
            binary = cargo_build(leg, target_dir,
                                 {"RUSTFLAGS": rustflags(leg)}, jobs, log)

        if leg.bolt:
            binary = bolt_optimize(binary, leg_dir, log)

        leg.bin_path = str(binary)
        leg.build_seconds = round(time.time() - t0, 1)
        leg.status = "ok"
    except subprocess.CalledProcessError as e:
        leg.status, leg.detail = "error", f"build failed (see {leg_dir}/build.log)"
        log(f"ERROR: {e}")
    finally:
        logf.close()
    return leg


def bolt_optimize(binary: Path, leg_dir: Path, log) -> Path:
    """Instrument → workload → optimize the ELF layout post-link."""
    inst = leg_dir / f"{BIN}.inst"
    fdata = leg_dir / "bolt.fdata"
    subprocess.run(
        ["llvm-bolt", str(binary), "-instrument",
         f"-instrumentation-file={fdata}", "-instrumentation-file-append-pid",
         "-o", str(inst)],
        check=True, stdout=log.file, stderr=subprocess.STDOUT)
    run_workload(inst, leg_dir, log, iterations=2)
    # merge-fdata across the per-pid drops.
    merged = leg_dir / "bolt.merged.fdata"
    drops = list(leg_dir.glob("bolt.fdata*"))
    with open(merged, "w") as out:
        subprocess.run(["merge-fdata", *[str(d) for d in drops]],
                       check=True, stdout=out, stderr=log.file)
    optimized = leg_dir / f"{BIN}.bolt"
    subprocess.run(
        ["llvm-bolt", str(binary), "-data", str(merged), "-o", str(optimized),
         "-reorder-blocks=ext-tsp", "-reorder-functions=hfsort",
         "-split-functions", "-split-all-cold", "-split-eh", "-icf=1",
         "-dyno-stats"],
        check=True, stdout=log.file, stderr=subprocess.STDOUT)
    # Re-strip: the non-BOLT legs inherit profile `strip = true`, so strip the
    # optimized output too or this leg's footprint numbers are inflated by the
    # symbol table BOLT required as input.
    stripper = shutil.which("llvm-strip") or shutil.which("strip")
    if stripper:
        subprocess.run([stripper, str(optimized)], check=False,
                       stdout=log.file, stderr=subprocess.STDOUT)
    return optimized


# ----------------------------------------------------------------------------
# Measure phase (serial)
# ----------------------------------------------------------------------------

def parse_size(binary: Path) -> tuple[int, int, int]:
    """Return (.text, .rodata, total) from `size -A`."""
    out = subprocess.check_output(["size", "-A", str(binary)], text=True)
    text = rodata = total = 0
    for line in out.splitlines():
        parts = line.split()
        if len(parts) >= 2 and parts[1].isdigit():
            name, sz = parts[0], int(parts[1])
            if name == ".text":
                text = sz
            elif name == ".rodata":
                rodata = sz
        if line.lower().startswith("total"):
            total = int(parts[1])
    return text, rodata, total


def measure_leg(leg: Leg, work: Path, pin: str | None, runs: int, log_print):
    binary = Path(leg.bin_path)
    log_print(f"  measure {leg.id} → {binary}")
    leg.file_bytes = binary.stat().st_size
    try:
        leg.text_bytes, leg.rodata_bytes, leg.total_size_bytes = parse_size(binary)
    except Exception as e:
        log_print(f"    size -A failed: {e}")

    # Runtime: cold `crabcc index` over a fresh fixture copy is the heaviest,
    # most alloc/IO-representative path. Reset .crabcc each run via --prepare.
    root = work / "_measure-root"
    if not root.exists():
        shutil.copytree(REPO_ROOT, root, ignore=shutil.ignore_patterns(
            ".git", "target", "bench", "node_modules", ".crabcc", "dist"))
    pin_prefix = (["taskset", "-c", pin] if pin else [])
    hj = work / f"{leg.id}.hyperfine.json"
    index_cmd = " ".join(pin_prefix + [shquote(str(binary)), "index", "--root", shquote(str(root))])
    try:
        subprocess.run([
            "hyperfine", "--warmup", "2", "--runs", str(runs),
            "--prepare", f"rm -rf {shquote(str(root / '.crabcc'))}",
            "--export-json", str(hj), index_cmd,
        ], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        data = json.loads(hj.read_text())["results"][0]
        leg.index_mean_s = round(data["mean"], 4)
        leg.index_stddev_s = round(data.get("stddev") or 0.0, 4)
    except Exception as e:
        log_print(f"    hyperfine(index) failed: {e}")

    # Warm query micro: index once, then time `sym`.
    try:
        subprocess.run([str(binary), "index", "--root", str(root)],
                       check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        qj = work / f"{leg.id}.query.json"
        query_cmd = " ".join(pin_prefix + [shquote(str(binary)), "sym", "Store", "--root", shquote(str(root))])
        subprocess.run([
            "hyperfine", "--warmup", "3", "--runs", str(max(runs, 20)),
            "--export-json", str(qj), query_cmd,
        ], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        leg.query_mean_s = round(json.loads(qj.read_text())["results"][0]["mean"], 5)
    except Exception as e:
        log_print(f"    hyperfine(query) failed: {e}")


def shquote(s: str) -> str:
    return "'" + s.replace("'", "'\\''") + "'" if any(c in s for c in " '\"") else s


# ----------------------------------------------------------------------------
# Reporting
# ----------------------------------------------------------------------------

def render_report(legs: list[Leg], meta: dict) -> str:
    ok = [l for l in legs if l.status == "ok"]
    by_speed = sorted([l for l in ok if l.index_mean_s is not None],
                      key=lambda l: l.index_mean_s)
    by_size = sorted([l for l in ok if l.file_bytes is not None],
                     key=lambda l: l.file_bytes)
    lines = [
        "# bench-opt-bin — optimization sweep report",
        "",
        f"- generated: {meta['generated']}",
        f"- host: {meta['host']}  ·  cores: {meta['cores']}  ·  pool: {meta['jobs']}",
        f"- profile: `{PROFILE}`  ·  legs: {len(legs)} "
        f"(ok {len(ok)}, skipped {sum(l.status=='skipped' for l in legs)}, "
        f"error {sum(l.status=='error' for l in legs)})",
        f"- wall time: {meta['wall_seconds']}s",
        "",
        "## Execution speed (cold `crabcc index`, lower is better)",
        "",
        "| leg | alloc | cpu | pgo | bolt | index mean (s) | ±stddev | query (s) | build (s) |",
        "|---|---|---|:--:|:--:|--:|--:|--:|--:|",
    ]
    base = next((l for l in by_speed if l.id == "baseline-v3-system"), None)
    for l in by_speed:
        delta = ""
        if base and base.index_mean_s and l.index_mean_s:
            pct = (l.index_mean_s - base.index_mean_s) / base.index_mean_s * 100
            delta = f" ({pct:+.1f}%)"
        lines.append(
            f"| `{l.id}` | {l.alloc} | {l.target_cpu} | {'✓' if l.pgo else ''} | "
            f"{'✓' if l.bolt else ''} | {l.index_mean_s}{delta} | {l.index_stddev_s} | "
            f"{l.query_mean_s if l.query_mean_s is not None else '—'} | {l.build_seconds} |")
    lines += [
        "",
        "## Binary footprint (lower is better)",
        "",
        "| leg | file (KiB) | .text (KiB) | .rodata (KiB) | total (KiB) |",
        "|---|--:|--:|--:|--:|",
    ]
    for l in by_size:
        kib = lambda b: f"{b/1024:.0f}" if b else "—"
        lines.append(
            f"| `{l.id}` | {kib(l.file_bytes)} | {kib(l.text_bytes)} | "
            f"{kib(l.rodata_bytes)} | {kib(l.total_size_bytes)} |")

    # Winner callout + paste-ready config.
    if by_speed:
        w = by_speed[0]
        feats = ALLOC_FEATURES[w.alloc]
        rf = rustflags(w).replace(' -C profile-generate', '').strip()
        lines += [
            "",
            "## Fastest config",
            "",
            f"**`{w.id}`** — index {w.index_mean_s}s"
            + (f", {((w.index_mean_s-base.index_mean_s)/base.index_mean_s*100):+.1f}% vs baseline"
               if base and base.index_mean_s else "") + ".",
            "",
            "Reproduce / promote into the `release-nightly` lane:",
            "",
            "```bash",
            f"export RUSTFLAGS=\"{rf}\"",
            f"cargo build --profile {PROFILE} -p {PKG} --bin {BIN}"
            + (f" --features {','.join(feats)}" if feats else ""),
        ]
        if w.pgo:
            lines.append("# (+ PGO: see scripts/bench-opt-bin/README.md for the instrument→use loop)")
        if w.bolt:
            lines.append("# (+ BOLT post-link: llvm-bolt with the recorded fdata)")
        lines.append("```")

    skipped = [l for l in legs if l.status in ("skipped", "error")]
    if skipped:
        lines += ["", "## Skipped / errored", ""]
        for l in skipped:
            lines.append(f"- `{l.id}` — **{l.status}**: {l.detail}")
    lines.append("")
    return "\n".join(lines)


# ----------------------------------------------------------------------------
# Flamegraphs + archival
# ----------------------------------------------------------------------------

def capture_flamegraphs(built: list[Leg], work: Path, flame_dir: Path,
                        jobs: int, log_print) -> list[Path]:
    """Render symbolized flamegraph SVGs for the baseline + fastest legs.

    The sweep binaries are stripped (release profile), so we rebuild the two
    legs of interest under the `profiling` profile (LTO + debug info, no strip
    — same one `task flamegraph-index` uses) so perf can resolve symbols."""
    if not (have("flamegraph") and have("perf")):
        log_print("   flamegraph: skipped (need cargo-flamegraph + perf on PATH)")
        return []
    ok = [l for l in built if l.status == "ok" and l.index_mean_s is not None]
    if not ok:
        return []
    fastest = min(ok, key=lambda l: l.index_mean_s)
    baseline = next((l for l in ok if l.id.startswith("baseline")), None)
    targets = {l.id: l for l in (baseline, fastest) if l}
    root = work / "_measure-root"
    flame_dir.mkdir(parents=True, exist_ok=True)
    svgs: list[Path] = []
    for lid, leg in targets.items():
        svg = flame_dir / f"{lid}.svg"
        feats = ALLOC_FEATURES[leg.alloc]
        cmd = ["flamegraph", "--profile", "profiling", "-p", PKG,
               "--bin", BIN, "-o", str(svg)]
        if feats:
            cmd += ["--features", ",".join(feats)]
        cmd += ["--", "index", "--root", str(root)]
        env = {**os.environ,
               "RUSTFLAGS": f"-C target-cpu={leg.target_cpu}",
               "CARGO_TARGET_DIR": str(work / f"flame-{lid}" / "target"),
               "CARGO_BUILD_JOBS": str(jobs)}
        try:
            shutil.rmtree(root / ".crabcc", ignore_errors=True)
            subprocess.run(cmd, cwd=REPO_ROOT, env=env, check=True,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            if svg.exists():
                svgs.append(svg)
                log_print(f"   flamegraph: {svg}")
        except Exception as e:
            log_print(f"   flamegraph({lid}) failed: {e}")
    return svgs


def _sha256(p: Path) -> str:
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 16), b""):
            h.update(chunk)
    return h.hexdigest()


def _targz(dest: Path, src_dir: Path):
    with tarfile.open(dest, "w:gz") as tar:
        tar.add(src_dir, arcname=src_dir.name)


def archive_run(built, meta, out: Path, work: Path, flame_dir: Path,
                archive_dir: str | None, log_print) -> Path:
    """Bundle every generated artifact into a timestamped, self-contained run
    dir + tarball + MANIFEST (sha256 per file). Optionally mirror the curated
    subset into a tracked dir so `git commit` durably backs the run up."""
    ts = re.sub(r"[^0-9]", "", meta["generated"])
    runid = f"run-{meta['host']}-{ts}"
    rd = out / runid
    (rd / "logs").mkdir(parents=True, exist_ok=True)
    (rd / "hyperfine").mkdir(exist_ok=True)

    shutil.copy(out / "opt-bin-REPORT.md", rd / "REPORT.md")
    shutil.copy(out / "opt-bin.ndjson", rd / "opt-bin.ndjson")
    for leg in built:
        lg = work / leg.id / "build.log"
        if lg.exists():
            shutil.copy(lg, rd / "logs" / f"{leg.id}.log")
    for pat in ("*.hyperfine.json", "*.query.json"):
        for j in work.glob(pat):
            shutil.copy(j, rd / "hyperfine" / j.name)
    if flame_dir.exists() and any(flame_dir.iterdir()):
        shutil.copytree(flame_dir, rd / "flamegraphs", dirs_exist_ok=True)

    artifacts = {str(p.relative_to(rd)): {"bytes": p.stat().st_size,
                                          "sha256": _sha256(p)}
                 for p in sorted(rd.rglob("*")) if p.is_file()}
    manifest = {**meta, "run_id": runid,
                "legs": [asdict(l) for l in built], "artifacts": artifacts}
    (rd / "MANIFEST.json").write_text(json.dumps(manifest, indent=2))

    tarball = out / f"{runid}.tar.gz"
    _targz(tarball, rd)
    log_print(f"   archive dir: {rd}")
    log_print(f"   tarball:     {tarball}")

    if archive_dir:
        adir = Path(archive_dir) / runid
        (adir).mkdir(parents=True, exist_ok=True)
        for name in ("REPORT.md", "opt-bin.ndjson", "MANIFEST.json"):
            shutil.copy(rd / name, adir / name)
        if (rd / "flamegraphs").exists():
            shutil.copytree(rd / "flamegraphs", adir / "flamegraphs",
                            dirs_exist_ok=True)
        _targz(adir / "logs.tar.gz", rd / "logs")
        log_print(f"   tracked backup: {adir}  (commit to persist)")
    return rd


# ----------------------------------------------------------------------------
# Main
# ----------------------------------------------------------------------------

def main() -> int:
    ap = argparse.ArgumentParser(description="bench-opt-bin optimization sweep")
    ap.add_argument("--work", default=str(REPO_ROOT / "bench" / "opt-bin"),
                    help="scratch dir for per-leg builds (default: bench/opt-bin)")
    ap.add_argument("--out", default=str(REPO_ROOT / "bench" / "results"),
                    help="dir for NDJSON + report (default: bench/results)")
    ap.add_argument("--jobs", type=int, default=0,
                    help="parallel build legs (default: saturate — min(legs, cores))")
    ap.add_argument("--runs", type=int, default=8, help="hyperfine runs per leg")
    ap.add_argument("--pin", default=None,
                    help="taskset core spec for measurements, e.g. '0-3'")
    ap.add_argument("--only", default=None,
                    help="comma list of leg ids to run (default: all)")
    ap.add_argument("--deep", action="store_true",
                    help="wider ~28-leg matrix (target-cpu axis) — keeps a big "
                         "box busy through the serial LTO phase")
    ap.add_argument("--saturate", action="store_true", default=True,
                    help="oversubscribe per-leg build jobs to fill idle cores "
                         "during the single-threaded LTO link (default: on)")
    ap.add_argument("--no-saturate", dest="saturate", action="store_false",
                    help="size per-leg jobs to exactly cores (no oversubscription)")
    ap.add_argument("--quick", action="store_true",
                    help="smoke: baseline + one mimalloc leg, no PGO/BOLT")
    ap.add_argument("--allow-missing-tools", action="store_true",
                    help="build even if hyperfine/size are absent (measurements "
                         "will be skipped — normally the run aborts instead)")
    ap.add_argument("--flamegraph", action="store_true",
                    help="render symbolized flamegraph SVGs for baseline + "
                         "fastest leg (rebuilds them under the `profiling` "
                         "profile; needs cargo-flamegraph + perf)")
    ap.add_argument("--archive-dir", default=None,
                    help="ALSO mirror curated artifacts (report, ndjson, "
                         "manifest, flamegraphs, gzipped logs) into this dir — "
                         "point it at a tracked path to back the run up via git")
    ap.add_argument("--dry-run", action="store_true", help="print the plan and exit")
    args = ap.parse_args()

    # --quick is a fixed 2-leg smoke, independent of --deep — the deep matrix
    # renames the smoke candidates, so filtering by id would drop every leg.
    if args.quick:
        legs = [Leg("baseline-v3-system", "x86-64-v3", "system"),
                Leg("v3-mimalloc", "x86-64-v3", "mimalloc")]
    else:
        legs = deep_matrix() if args.deep else default_matrix()
    if args.only:
        want = set(args.only.split(","))
        legs = [l for l in legs if l.id in want]
        if not legs:
            print(f"no legs match --only={args.only}", file=sys.stderr)
            return 2

    # Saturating job math. Fat-LTO's final link is largely single-threaded, so
    # to keep every core busy we run ~one leg per core (capped by leg count)
    # rather than a handful of wide builds. During the parallel dep-codegen
    # bursts that briefly oversubscribes — harmless and intended; it's what
    # fills the cores the LTO links leave idle. Deepen the matrix (--deep) when
    # legs < cores so there's enough independent work to stay full.
    cores = os.cpu_count() or 4
    n = len(legs)
    jobs = args.jobs if args.jobs > 0 else max(1, min(n, cores))
    oversub = 1.5 if args.saturate else 1.0
    per_leg_jobs = max(2, math.ceil(cores * oversub / jobs))

    util_note = ("" if n >= cores else
                 f"  ⚠ {n} legs < {cores} cores: LTO links will under-fill the box; "
                 "use --deep or a smaller flavor")
    if args.dry_run:
        print(f"host cores={cores} legs={n} pool={jobs} per-leg-jobs={per_leg_jobs} "
              f"saturate={args.saturate}{util_note}")
        for l in legs:
            print(f"  {l.id:24s} cpu={l.target_cpu:10s} alloc={l.alloc:8s} "
                  f"optz={'z' if l.opt_level else '-'} pgo={int(l.pgo)} bolt={int(l.bolt)}")
        return 0

    work = Path(args.work); work.mkdir(parents=True, exist_ok=True)
    out = Path(args.out); out.mkdir(parents=True, exist_ok=True)

    profdata = llvm_profdata()
    bolt_ok = have("llvm-bolt") and have("merge-fdata")
    need_bolt = any(l.bolt for l in legs)
    need_pgo = any(l.pgo for l in legs)
    print(f"== bench-opt-bin == cores={cores} legs={n} pool={jobs} "
          f"per-leg-jobs={per_leg_jobs} saturate={args.saturate}{util_note}")
    print(f"   hyperfine={'ok' if have('hyperfine') else 'MISSING'} "
          f"size={'ok' if have('size') else 'MISSING'} "
          f"llvm-profdata={'ok' if profdata else 'MISSING'} "
          f"bolt={'ok' if bolt_ok else 'MISSING'} "
          f"sccache={'ok' if have('sccache') else 'off'}")
    if need_pgo and not profdata:
        print("   ! PGO legs will be skipped (install: rustup component add llvm-tools-preview)")
    if need_bolt and not bolt_ok:
        print("   ! BOLT legs will be skipped (install: llvm-bolt + merge-fdata)")
    # hyperfine + `size` produce the entire point of the run (timing + footprint).
    # Without them every leg's measurement fails and we'd emit an empty-looking
    # but "successful" report after an expensive build phase — abort up front
    # instead. --allow-missing-tools opts out (e.g. to validate the build legs).
    missing = [t for t in ("hyperfine", "size") if not have(t)]
    if missing and not args.allow_missing_tools:
        print(f"   ! required measurement tool(s) missing: {', '.join(missing)}.\n"
              f"     install them (hyperfine; binutils for `size`) or pass "
              f"--allow-missing-tools to build without measuring.", file=sys.stderr)
        return 2

    started = time.time()

    # Phase A — parallel builds.
    print(f"\n-- phase A: building {len(legs)} legs ({jobs}-wide) --")
    built: list[Leg] = []
    with ThreadPoolExecutor(max_workers=jobs) as pool:
        futs = {pool.submit(build_leg, l, work, per_leg_jobs, profdata, bolt_ok): l
                for l in legs}
        for fut in as_completed(futs):
            leg = fut.result()
            built.append(leg)
            print(f"   [{leg.status:7s}] {leg.id}"
                  + (f"  build={leg.build_seconds}s" if leg.build_seconds else "")
                  + (f"  ({leg.detail})" if leg.detail else ""))

    # Phase B — serial measurement.
    print(f"\n-- phase B: measuring (serial{', pinned '+args.pin if args.pin else ''}) --")
    for leg in sorted([l for l in built if l.status == "ok"], key=lambda l: l.id):
        measure_leg(leg, work, args.pin, args.runs, print)

    meta = {
        "generated": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "host": os.uname().nodename,
        "cores": cores,
        "jobs": jobs,
        "wall_seconds": round(time.time() - started, 1),
    }

    ndjson = out / "opt-bin.ndjson"
    with open(ndjson, "w") as f:
        for leg in built:
            f.write(json.dumps({**meta, **asdict(leg)}) + "\n")
    report = out / "opt-bin-REPORT.md"
    report.write_text(render_report(built, meta))
    print(f"\n== done in {meta['wall_seconds']}s ==")
    print(f"   NDJSON: {ndjson}")
    print(f"   report: {report}")

    # Optional symbolized flamegraphs (baseline + fastest).
    flame_dir = work / "flamegraphs"
    if args.flamegraph:
        print("\n-- flamegraphs --")
        capture_flamegraphs(built, work, flame_dir, per_leg_jobs, print)

    # Bundle + (optionally) mirror into a tracked dir so nothing is lost when
    # the box / container is reclaimed.
    print("\n-- archiving --")
    archive_run(built, meta, out, work, flame_dir, args.archive_dir, print)
    return 0


if __name__ == "__main__":
    sys.exit(main())
