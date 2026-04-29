#!/usr/bin/env python3
"""Read claude -p JSON transcripts under bench/results and emit a savings table.

Each result file is named <task-id>-<mode>.json where mode is raw|crabcc.
Primary metric is total_cost_usd (claude reports it directly). We also surface
total tokens (cost-weighted = sum of input + cache_creation + cache_read*0.1
+ output*5) so the saving is interpretable when costs aren't logged."""
import json
import sys
from pathlib import Path
from collections import defaultdict


def load_record(path: Path) -> dict:
    try:
        d = json.loads(path.read_text())
    except (json.JSONDecodeError, OSError) as e:
        return {"error": str(e)}
    u = d.get("usage") or {}
    return {
        "error": None,
        "cost_usd":     float(d.get("total_cost_usd", 0) or 0),
        "duration_ms":  int(d.get("duration_ms", 0) or 0),
        "num_turns":    int(d.get("num_turns", 0) or 0),
        "input_tokens":           int(u.get("input_tokens", 0) or 0),
        "cache_creation_tokens":  int(u.get("cache_creation_input_tokens", 0) or 0),
        "cache_read_tokens":      int(u.get("cache_read_input_tokens", 0) or 0),
        "output_tokens":          int(u.get("output_tokens", 0) or 0),
        "result_text":  str(d.get("result", ""))[:120],
    }


def cost_weighted_tokens(r: dict) -> int:
    """Approximate per-call cost in 'effective tokens' relative to base input.
    Output is ~5x base input, cache_creation ~1x, cache_read ~0.1x."""
    return (
        r["input_tokens"]
        + r["cache_creation_tokens"]
        + int(r["cache_read_tokens"] * 0.1)
        + r["output_tokens"] * 5
    )


def fmt_money(x: float) -> str:
    return f"${x:.4f}"


def fmt_savings(raw: float, cc: float) -> str:
    if raw == 0:
        return "—"
    return f"{(1 - cc / raw) * 100:+5.1f}%"


def main(results_dir: str):
    root = Path(results_dir)
    if not root.is_dir():
        print(f"no results dir: {root}", file=sys.stderr); sys.exit(1)

    by_task = defaultdict(dict)
    for p in sorted(root.glob("*.json")):
        if p.name == "summary.json":
            continue
        stem = p.stem
        if stem.endswith("-raw"):
            task, mode = stem[:-4], "raw"
        elif stem.endswith("-crabcc"):
            task, mode = stem[:-7], "crabcc"
        else:
            continue
        by_task[task][mode] = load_record(p)

    width = max([14] + [len(t) for t in by_task])
    print(f"{'task'.ljust(width)} | {'mode':>6} | {'cost':>9} | {'turns':>5} | {'in':>6} | {'cache_c':>8} | {'cache_r':>8} | {'out':>6} | {'$ savings':>9}")
    print("-" * (width + 80))

    tot_cost  = {"raw": 0.0, "crabcc": 0.0}
    tot_eff   = {"raw": 0,   "crabcc": 0}

    for task in sorted(by_task):
        m = by_task[task]
        raw = m.get("raw"); cc = m.get("crabcc")
        if not raw or not cc:
            print(f"{task.ljust(width)} | (one mode missing — skip)")
            continue
        tot_cost["raw"]    += raw["cost_usd"]
        tot_cost["crabcc"] += cc["cost_usd"]
        tot_eff["raw"]     += cost_weighted_tokens(raw)
        tot_eff["crabcc"]  += cost_weighted_tokens(cc)
        sav = fmt_savings(raw["cost_usd"], cc["cost_usd"])
        for mode, r in (("raw", raw), ("crabcc", cc)):
            print(f"{task.ljust(width)} | {mode:>6} | {fmt_money(r['cost_usd']):>9} | "
                  f"{r['num_turns']:>5} | {r['input_tokens']:>6,} | "
                  f"{r['cache_creation_tokens']:>8,} | {r['cache_read_tokens']:>8,} | "
                  f"{r['output_tokens']:>6,} | {sav if mode=='raw' else '':>9}")

    print("-" * (width + 80))
    overall_savings = fmt_savings(tot_cost["raw"], tot_cost["crabcc"])
    eff_savings     = fmt_savings(tot_eff["raw"],  tot_eff["crabcc"])
    print(f"{'TOTAL'.ljust(width)} | {'raw':>6} | {fmt_money(tot_cost['raw']):>9}")
    print(f"{'TOTAL'.ljust(width)} | {'crabcc':>6} | {fmt_money(tot_cost['crabcc']):>9}  {overall_savings} cost  ({eff_savings} eff. tokens)")

    summary = root / "summary.json"
    summary.write_text(json.dumps(
        {"per_task": dict(by_task),
         "totals": {"cost_usd": tot_cost, "effective_tokens": tot_eff}},
        indent=2))
    print(f"\nfull summary: {summary}")


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "bench/results")
