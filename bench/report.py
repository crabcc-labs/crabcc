#!/usr/bin/env python3
"""Read claude -p JSON transcripts under bench/results and emit a savings table.

Each result file is named <task-id>-<mode>.json where mode is raw|crabcc.
We extract the usage block, sum input + output tokens, and compare per-task
plus a grand total."""
import json
import os
import sys
from pathlib import Path
from collections import defaultdict


def load_usage(path: Path) -> dict:
    """Pull token counts out of a `claude -p --output-format json` result.
    The exact shape varies by CLI version; we look in a few likely places."""
    try:
        data = json.loads(path.read_text())
    except (json.JSONDecodeError, OSError) as e:
        return {"error": str(e), "input_tokens": 0, "output_tokens": 0,
                "cache_read": 0, "cache_creation": 0}

    usage = data.get("usage") or data.get("total_usage") or {}
    if not usage and isinstance(data.get("messages"), list):
        for m in data["messages"]:
            if isinstance(m, dict) and m.get("usage"):
                usage = m["usage"]; break

    return {
        "error": None,
        "input_tokens":  int(usage.get("input_tokens",  0) or 0),
        "output_tokens": int(usage.get("output_tokens", 0) or 0),
        "cache_read":    int(usage.get("cache_read_input_tokens",     0) or 0),
        "cache_creation":int(usage.get("cache_creation_input_tokens", 0) or 0),
        "duration_ms":   int(data.get("duration_ms", 0) or 0),
        "num_turns":     int(data.get("num_turns",   0) or 0),
        "result_text":   str(data.get("result", ""))[:120],
    }


def total(u: dict) -> int:
    return u["input_tokens"] + u["output_tokens"]


def main(results_dir: str):
    root = Path(results_dir)
    if not root.is_dir():
        print(f"no results dir: {root}", file=sys.stderr); sys.exit(1)

    by_task = defaultdict(dict)
    for p in sorted(root.glob("*.json")):
        stem = p.stem
        if "-raw" in stem:
            task = stem[:-len("-raw")]; mode = "raw"
        elif "-crabcc" in stem:
            task = stem[:-len("-crabcc")]; mode = "crabcc"
        else:
            continue
        by_task[task][mode] = load_usage(p)

    rows, totals = [], {"raw": 0, "crabcc": 0}
    for task, modes in by_task.items():
        raw = modes.get("raw"); cc = modes.get("crabcc")
        if not raw or not cc:
            rows.append((task, "—", "—", "—", "(missing one mode)")); continue
        r, c = total(raw), total(cc)
        totals["raw"] += r; totals["crabcc"] += c
        savings = "—" if r == 0 else f"{(1 - c / r) * 100:5.1f}%"
        note = ""
        if raw["error"] or cc["error"]:
            note = "(error)"
        rows.append((task, f"{r:>8,}", f"{c:>8,}", savings, note))

    width = max([20] + [len(t) for t, *_ in rows])
    print(f"{'task'.ljust(width)}  {'raw':>9}  {'crabcc':>9}  {'savings':>8}  note")
    print("-" * (width + 40))
    for t, r, c, s, note in rows:
        print(f"{t.ljust(width)}  {r:>9}  {c:>9}  {s:>8}  {note}")
    print("-" * (width + 40))

    if totals["raw"]:
        agg = f"{(1 - totals['crabcc'] / totals['raw']) * 100:5.1f}%"
        print(f"{'TOTAL'.ljust(width)}  {totals['raw']:>9,}  {totals['crabcc']:>9,}  {agg:>8}")

    summary = root / "summary.json"
    summary.write_text(json.dumps(
        {"per_task": {t: m for t, m in by_task.items()}, "totals": totals},
        indent=2))
    print(f"\nfull summary: {summary}")


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "bench/results")
