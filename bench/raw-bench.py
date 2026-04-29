#!/usr/bin/env python3
"""
First-layer benchmark: crabcc CLI vs raw grep/find/ls equivalents.

NOT a Claude-session benchmark — measures only what the agent's stdout
buffer would see (bytes, ~tokens) and wall-time. The premise: if crabcc
beats grep/find on bytes-out and ms at the shell level, it must beat them
inside a Claude session too (modulo per-turn cache cost, which only
matters when the tool causes extra turns).

For each task: runs both sides 3 times, takes the min wall-time
(reduces noise from disk cache warm-up, OS jitter), records bytes.

Usage:
  python3 bench/raw-bench.py <fixture-repo>
"""
from __future__ import annotations
import json
import shlex
import subprocess
import sys
import time
from pathlib import Path
from statistics import median

TRIALS = 3
TIMEOUT = 60

# Each task is one (id, prompt, crabcc_cmd, raw_cmd) tuple.
# raw_cmd is what an agent without crabcc would plausibly run.
# Each task has crabcc + two raw variants: classic (grep/find/cat) and modern
# (ripgrep/fd) — the fair comparison, since rg/fd are gitignore-aware like crabcc.
TASKS = [
    {
        "id":     "sym-User",
        "prompt": "where is the User class defined?",
        "crabcc": "crabcc sym User",
        "raw":    r"""grep -rnE 'class User\b|module User\b' --include='*.rb' --include='*.ts' --include='*.tsx' --include='*.js' .""",
        "rg":     r"""rg -n --type-add 'src:*.{rb,ts,tsx,js}' -tsrc 'class User\b|module User\b'""",
    },
    {
        "id":     "sym-Assessment",
        "prompt": "where is Assessment defined?",
        "crabcc": "crabcc sym Assessment",
        "raw":    r"""grep -rnE 'class Assessment\b|module Assessment\b' --include='*.rb' --include='*.ts' --include='*.tsx' --include='*.js' .""",
        "rg":     r"""rg -n --type-add 'src:*.{rb,ts,tsx,js}' -tsrc 'class Assessment\b|module Assessment\b'""",
    },
    {
        "id":     "callers-count-find_by",
        "prompt": "how many call sites of find_by?",
        "crabcc": "crabcc callers find_by --count",
        "raw":    r"""grep -rEoh '\bfind_by\(' --include='*.rb' --include='*.ts' --include='*.tsx' --include='*.js' . | wc -l""",
        "rg":     r"""rg --type-add 'src:*.{rb,ts,tsx,js}' -tsrc -oc '\bfind_by\(' | awk -F: '{s+=$2} END{print s}'""",
    },
    {
        "id":     "refs-files-Assessment",
        "prompt": "first 10 files that reference Assessment",
        "crabcc": "crabcc refs Assessment --files-only --limit 10",
        "raw":    r"""grep -rlE '\bAssessment\b' --include='*.rb' --include='*.ts' --include='*.tsx' --include='*.js' . | head -10""",
        "rg":     r"""rg --type-add 'src:*.{rb,ts,tsx,js}' -tsrc -l '\bAssessment\b' | head -10""",
    },
    {
        "id":     "outline-assessment-rb",
        "prompt": "outline of app/models/assessment.rb (top-level methods/classes)",
        "crabcc": "crabcc outline app/models/assessment.rb",
        "raw":    r"""grep -nE '^[[:space:]]*(class|module|def|attr_(reader|writer|accessor))\b' app/models/assessment.rb""",
        "rg":     r"""rg -n '^[[:space:]]*(class|module|def|attr_(reader|writer|accessor))\b' app/models/assessment.rb""",
    },
    {
        "id":     "outline-vs-read-assessment-rb",
        "prompt": "structure of app/models/assessment.rb — what Claude does today (Read whole file)",
        "crabcc": "crabcc outline app/models/assessment.rb",
        "raw":    r"""cat app/models/assessment.rb""",
        "rg":     r"""cat app/models/assessment.rb""",
    },
    {
        "id":     "files-models-rb",
        "prompt": "list all .rb files under app/models",
        "crabcc": "crabcc files --under app/models --ext rb",
        "raw":    r"""find app/models -type f -name '*.rb'""",
        # fd often isn't installed; fall back to rg --files which respects gitignore
        "rg":     r"""rg --files -g '*.rb' app/models""",
    },
    {
        "id":     "files-all-rb",
        "prompt": "list all .rb files in repo",
        "crabcc": "crabcc files --ext rb",
        "raw":    r"""find . -type f -name '*.rb' -not -path './node_modules/*' -not -path './tmp/*' -not -path './.git/*'""",
        "rg":     r"""rg --files -g '*.rb'""",
    },
    {
        "id":     "callers-files-find_by",
        "prompt": "which files contain calls to find_by (deduped)",
        "crabcc": "crabcc callers find_by --files-only --limit 20",
        "raw":    r"""grep -rlE '\bfind_by\(' --include='*.rb' --include='*.ts' --include='*.tsx' --include='*.js' . | head -20""",
        "rg":     r"""rg --type-add 'src:*.{rb,ts,tsx,js}' -tsrc -l '\bfind_by\(' | head -20""",
    },
]


def run_one(cmd: str, cwd: Path) -> tuple[int, float, int]:
    """Returns (bytes_out, wall_ms, exit_code)."""
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
    bytes_out = runs[-1][0]   # last trial — disk-cache-warm
    wall_ms   = min(r[1] for r in runs)  # min over warm trials
    rc        = runs[-1][2]
    return {
        "bytes":   bytes_out,
        "ms_min":  round(wall_ms, 1),
        "ms_med":  round(median(r[1] for r in runs), 1),
        "rc":      rc,
        "tokens_approx": bytes_out // 4 if bytes_out >= 0 else None,
    }


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: raw-bench.py <fixture-repo>", file=sys.stderr)
        return 2
    fixture = Path(sys.argv[1]).resolve()
    if not fixture.is_dir():
        print(f"error: {fixture} is not a directory", file=sys.stderr)
        return 2
    if not (fixture / ".crabcc" / "index.db").exists():
        print(f"error: {fixture} has no .crabcc index — run `crabcc index` first", file=sys.stderr)
        return 2

    results = []
    for t in TASKS:
        print(f"  [{t['id']}]", flush=True)
        crab = trials(t["crabcc"], fixture)
        raw  = trials(t["raw"],    fixture)
        rg   = trials(t["rg"],     fixture) if t.get("rg") else None
        results.append({
            "id":      t["id"],
            "prompt":  t["prompt"],
            "crabcc":  {"cmd": t["crabcc"], **crab},
            "raw":     {"cmd": t["raw"],    **raw},
            "rg":      ({"cmd": t["rg"], **rg} if rg else None),
        })

    here = Path(__file__).parent
    out_path = here / "results" / "raw.json"
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(results, indent=2))
    print(f"\nwrote {out_path}")

    # Inline summary table for the terminal — three columns wide so rg is visible.
    print()
    hdr = (f"{'task':<26} "
           f"{'crabcc B':>9} {'rg B':>9} {'grep B':>9} "
           f"{'crabcc ms':>10} {'rg ms':>9} {'grep ms':>9}  "
           f"{'vs rg':>8} {'vs grep':>8}")
    print(hdr)
    print("-" * len(hdr))
    for r in results:
        c, w = r["crabcc"], r["raw"]
        rg = r.get("rg") or {"bytes": -1, "ms_min": 0.0}
        sp_grep = w["ms_min"] / c["ms_min"] if c["ms_min"] > 0 else 0
        sp_rg   = rg["ms_min"] / c["ms_min"] if c["ms_min"] > 0 and rg["ms_min"] > 0 else 0
        print(f"{r['id']:<26} "
              f"{c['bytes']:>9} {rg['bytes']:>9} {w['bytes']:>9} "
              f"{c['ms_min']:>10.1f} {rg['ms_min']:>9.1f} {w['ms_min']:>9.1f}  "
              f"{sp_rg:>7.2f}x {sp_grep:>7.2f}x")
    return 0


if __name__ == "__main__":
    sys.exit(main())
