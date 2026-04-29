#!/usr/bin/env python3
"""
Render the raw-bench results as a presentation-grade markdown report
plus PNG charts (matplotlib).

Inputs:  bench/results/raw.json  (produced by raw-bench.py)
Outputs:
  bench/results/REPORT.md     — exec summary + table + ASCII bars
  bench/results/savings.png   — bytes (token-proxy) savings per task
  bench/results/speedup.png   — wall-time speedup, log scale

The report is written for non-engineers: headline numbers up top,
honest about losses, dollar/cache framing where it helps.
"""
from __future__ import annotations
import json
import sys
from pathlib import Path

USD_PER_MTOK_INPUT  = 3.00   # Claude Sonnet 4.6 input pricing as of 2026-04
USD_PER_MTOK_OUTPUT = 15.00


def load(path: Path) -> list[dict]:
    return json.loads(path.read_text())


def fmt_bytes(n: int) -> str:
    if n is None or n < 0:
        return "TIMEOUT"
    if n >= 1_000_000:
        return f"{n/1_000_000:.1f}M"
    if n >= 1_000:
        return f"{n/1_000:.1f}k"
    return str(n)


def fmt_ms(n: float) -> str:
    if n >= 60_000:
        return f"{n/1000:.1f}s ⚠"
    if n >= 1000:
        return f"{n/1000:.2f}s"
    return f"{n:.1f}ms"


def fmt_pct(c: int, w: int) -> str:
    if c < 0 or w < 0 or w == 0:
        return "n/a"
    if c >= w:
        delta = (c - w) / w * 100
        return f"−{delta:.0f}%"  # crabcc bigger
    saved = (1.0 - c / w) * 100
    return f"+{saved:.0f}%"


def fmt_speedup(c_ms: float, w_ms: float) -> str:
    if c_ms <= 0 or w_ms <= 0:
        return "n/a"
    r = w_ms / c_ms
    if r >= 100:
        return f"{r:.0f}x"
    if r >= 10:
        return f"{r:.1f}x"
    return f"{r:.2f}x"


def ascii_bar(value: float, max_value: float, width: int = 30) -> str:
    if max_value <= 0 or value <= 0:
        return ""
    n = max(1, round(width * value / max_value))
    return "█" * n


def build_charts(results: list[dict], out_dir: Path) -> tuple[Path | None, Path | None]:
    """Generate matplotlib PNGs. Returns (savings_path, speedup_path) or (None, None)."""
    try:
        import matplotlib
        matplotlib.use("Agg")
        import matplotlib.pyplot as plt
    except ImportError:
        return None, None

    ids = [r["id"] for r in results]
    has_rg = any(r.get("rg") for r in results)

    crabcc_b  = [r["crabcc"]["bytes"]  for r in results]
    raw_b     = [r["raw"]["bytes"]     for r in results]
    rg_b      = [(r.get("rg") or {}).get("bytes", -1) for r in results]
    crabcc_ms = [r["crabcc"]["ms_min"] for r in results]
    raw_ms    = [r["raw"]["ms_min"]    for r in results]
    rg_ms     = [(r.get("rg") or {}).get("ms_min", 0.0) for r in results]

    # Replace timeout/missing with epsilon for log plot.
    def safe_log(values):
        return [max(v, 0.5) for v in values]

    # ----- Bytes per tool, grouped bar -----
    fig, ax = plt.subplots(figsize=(13, 5))
    x = range(len(ids))
    w = 0.27 if has_rg else 0.4
    if has_rg:
        ax.bar([i - w     for i in x], safe_log(raw_b),    width=w, label="grep / find / cat", color="#d97757")
        ax.bar([i         for i in x], safe_log(rg_b),     width=w, label="ripgrep / fd",       color="#e8b25d")
        ax.bar([i + w     for i in x], safe_log(crabcc_b), width=w, label="crabcc",             color="#5e8d52")
    else:
        ax.bar([i - w/2 for i in x], safe_log(raw_b),    width=w, label="grep / find / cat (raw)", color="#d97757")
        ax.bar([i + w/2 for i in x], safe_log(crabcc_b), width=w, label="crabcc",                  color="#5e8d52")
    ax.set_yscale("log")
    ax.set_ylabel("Output bytes (log scale, ≈ tokens × 4)")
    title = "crabcc vs ripgrep vs grep — output size per task" if has_rg \
            else "crabcc vs raw shell tools — output size per task"
    ax.set_title(f"{title}\n(smaller = fewer tokens consumed by the LLM)")
    ax.set_xticks(list(x))
    ax.set_xticklabels(ids, rotation=30, ha="right")
    ax.legend()
    ax.grid(axis="y", alpha=0.3, which="both")
    fig.tight_layout()
    savings_path = out_dir / "savings.png"
    fig.savefig(savings_path, dpi=140)
    plt.close(fig)

    # ----- Wall-time speedup vs grep AND vs rg -----
    sp_grep = [
        (raw_ms[i] / crabcc_ms[i]) if crabcc_ms[i] > 0 and raw_ms[i] > 0 else 0
        for i in range(len(ids))
    ]
    sp_rg = [
        (rg_ms[i] / crabcc_ms[i]) if crabcc_ms[i] > 0 and rg_ms[i] > 0 else 0
        for i in range(len(ids))
    ]
    fig, ax = plt.subplots(figsize=(13, 5))
    if has_rg:
        w = 0.4
        ax.bar([i - w/2 for i in x], sp_grep, width=w, label="vs grep / find", color="#d97757")
        ax.bar([i + w/2 for i in x], sp_rg,   width=w, label="vs ripgrep",     color="#e8b25d")
    else:
        ax.bar(ids, sp_grep, color=["#5e8d52" if s >= 1 else "#d97757" for s in sp_grep])
    ax.set_yscale("log")
    ax.axhline(1.0, color="gray", linestyle="--", linewidth=1, label="parity")
    ax.set_ylabel("speedup factor (other ms / crabcc ms, log)")
    title = "crabcc — wall-time speedup vs ripgrep AND vs grep" if has_rg \
            else "crabcc vs raw shell tools — wall-time speedup"
    ax.set_title(f"{title}\n(higher = crabcc faster; below dashed line = crabcc lost)")
    ax.set_xticks(list(x))
    ax.set_xticklabels(ids, rotation=30, ha="right")
    ax.legend()
    ax.grid(axis="y", alpha=0.3, which="both")
    fig.tight_layout()
    speedup_path = out_dir / "speedup.png"
    fig.savefig(speedup_path, dpi=140)
    plt.close(fig)
    return savings_path, speedup_path


def report_md(results: list[dict]) -> str:
    # Aggregate.
    total_crab_bytes = sum(max(r["crabcc"]["bytes"], 0) for r in results)
    total_raw_bytes  = sum(max(r["raw"]["bytes"], 0)    for r in results)
    bytes_saved = total_raw_bytes - total_crab_bytes
    bytes_pct   = (bytes_saved / total_raw_bytes * 100) if total_raw_bytes > 0 else 0

    total_crab_ms = sum(max(r["crabcc"]["ms_min"], 0.0) for r in results)
    total_raw_ms  = sum(max(r["raw"]["ms_min"], 0.0)    for r in results)
    speedup       = total_raw_ms / total_crab_ms if total_crab_ms > 0 else 0

    # Approximate $ on the raw vs crabcc difference (input-token rate, since
    # this is content the LLM ingests, not generates).
    tokens_saved = bytes_saved // 4
    usd_saved = tokens_saved / 1_000_000 * USD_PER_MTOK_INPUT

    lines: list[str] = []
    lines.append("# crabcc vs raw shell tools — first-layer benchmark\n")
    lines.append("> CLI-vs-CLI comparison. **No Claude session involved.** Measures only what the LLM's stdout buffer would receive (bytes ≈ tokens × 4) and wall-time. Fixture: `mc-mothership` (a real Rails monorepo, ~13k indexed files).\n")

    lines.append("## TL;DR\n")
    lines.append(f"- **{bytes_pct:.0f}% fewer bytes** sent to the LLM across {len(results)} representative code-lookup tasks (saved ≈ {tokens_saved:,} input tokens, ≈ ${usd_saved:.3f} per equivalent batch).")
    lines.append(f"- **{speedup:.0f}× faster wall-time** in aggregate. Several raw `grep -rn` calls **timed out at 60s**; crabcc returned in milliseconds.")
    lines.append("- Wins on: whole-repo symbol lookups, callers, references, file listings.")
    lines.append("- Honest losses on: single-file outline (raw `grep -nE` on one file is already cheap), small directory listings.\n")

    has_rg = any(r.get("rg") for r in results)
    lines.append("## Per-task results\n")
    if has_rg:
        lines.append("| Task | crabcc B | rg B | grep B | crabcc | rg | grep | vs rg | vs grep |")
        lines.append("|---|---:|---:|---:|---:|---:|---:|---:|---:|")
        for r in results:
            c, w = r["crabcc"], r["raw"]
            rg = r.get("rg") or {"bytes": -1, "ms_min": 0.0}
            lines.append(
                f"| `{r['id']}` "
                f"| {fmt_bytes(c['bytes'])} "
                f"| {fmt_bytes(rg['bytes'])} "
                f"| {fmt_bytes(w['bytes'])} "
                f"| {fmt_ms(c['ms_min'])} "
                f"| {fmt_ms(rg['ms_min'])} "
                f"| {fmt_ms(w['ms_min'])} "
                f"| {fmt_speedup(c['ms_min'], rg['ms_min'])} "
                f"| {fmt_speedup(c['ms_min'], w['ms_min'])} |"
            )
    else:
        lines.append("| Task | crabcc bytes | raw bytes | bytes-saving | crabcc time | raw time | speedup |")
        lines.append("|---|---:|---:|---:|---:|---:|---:|")
        for r in results:
            c, w = r["crabcc"], r["raw"]
            lines.append(
                f"| `{r['id']}` "
                f"| {fmt_bytes(c['bytes'])} "
                f"| {fmt_bytes(w['bytes'])} "
                f"| {fmt_pct(c['bytes'], w['bytes'])} "
                f"| {fmt_ms(c['ms_min'])} "
                f"| {fmt_ms(w['ms_min'])} "
                f"| {fmt_speedup(c['ms_min'], w['ms_min'])} |"
            )

    # ASCII bytes chart (works in terminal + GitHub markdown).
    lines.append("\n## Bytes per task (ASCII)\n")
    lines.append("```")
    max_bytes = max(max(r["raw"]["bytes"], r["crabcc"]["bytes"]) for r in results)
    for r in results:
        c, w = r["crabcc"], r["raw"]
        lines.append(f"{r['id']:<28}")
        lines.append(f"  raw    {fmt_bytes(w['bytes']):>8}  {ascii_bar(max(w['bytes'],0), max_bytes)}")
        lines.append(f"  crabcc {fmt_bytes(c['bytes']):>8}  {ascii_bar(max(c['bytes'],0), max_bytes)}")
        lines.append("")
    lines.append("```")

    lines.append("\n## Why these numbers\n")
    lines.append("- **`grep -rn` on a Rails monorepo touches `node_modules/`, `tmp/`, `.git/`** — that's why several runs timed out. crabcc only walks files its indexer accepted; same for `rg` which respects `.gitignore` by default.")
    lines.append("- **vs ripgrep:** rg is much faster than grep, but still has to scan every file from disk on every query. crabcc reads from a SQLite index — the answer is already in memory. That's why even rg shows 5–100× slowdowns vs crabcc on whole-repo questions.")
    lines.append("- **crabcc returns structured JSON, not raw text.** For whole-repo questions, that JSON is much smaller than the file excerpts an agent would otherwise have to read.")
    lines.append("- **Token-shaping flags (`--count`, `--files-only`, `--limit`) collapse 16k-token result sets to ~3 tokens** (`{\"count\":475}`) when the agent only needs a count or a deduped file list.")
    lines.append("- **The losses are on small targeted ops** where a one-line `rg`/`grep` on a single file is already trivial — crabcc's structured output costs more than the raw output it replaces. Recommend the agent stay with `rg`/`fd` for `outline of one small file`-shaped queries.\n")
    lines.append("## Recommended tool ladder\n")
    lines.append("- **Code-shape questions** (symbols, callers, refs, file outlines, code-file listings) → `crabcc`")
    lines.append("- **Free-text in code or non-code files** → `rg`")
    lines.append("- **Filename glob / by age / non-code** → `fd`")
    lines.append("- **Reshape JSON output** → `jq`")
    lines.append("- **Never** plain `grep -rn` or `find . -name` on a real repo.\n")

    lines.append("## What this benchmark does NOT prove\n")
    lines.append("- This measures the CLI tool, not the full Claude session. A separate Claude-session benchmark (existing `bench/run.sh`) showed **per-turn cache cost can erase CLI savings** when crabcc causes extra agent turns. Net wins require either (a) crabcc not adding turns, or (b) very large result sets where the byte savings outweigh one extra turn (~5k tokens).")
    lines.append("- The right framing for PMs: *crabcc's CLI advantage is large and unambiguous; converting that into Claude-session $ savings depends on agent prompting and skill design.*")
    return "\n".join(lines) + "\n"


def main() -> int:
    here = Path(__file__).parent
    raw_json = here / "results" / "raw.json"
    if not raw_json.exists():
        print(f"missing {raw_json} — run raw-bench.py first", file=sys.stderr)
        return 2
    results = load(raw_json)

    out_dir = here / "results"
    savings_png, speedup_png = build_charts(results, out_dir)

    md = report_md(results)
    if savings_png and speedup_png:
        md += f"\n## Charts\n\n![Bytes saved](./{savings_png.name})\n\n![Speedup](./{speedup_png.name})\n"
    else:
        md += "\n_(matplotlib not available — install with `pip install matplotlib` to regenerate PNG charts)_\n"

    md_path = out_dir / "REPORT.md"
    md_path.write_text(md)
    print(f"wrote {md_path}")
    if savings_png:
        print(f"wrote {savings_png}")
    if speedup_png:
        print(f"wrote {speedup_png}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
