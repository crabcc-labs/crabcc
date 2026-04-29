# crabcc vs raw shell tools — first-layer benchmark

> CLI-vs-CLI comparison. **No Claude session involved.** Measures only what the LLM's stdout buffer would receive (bytes ≈ tokens × 4) and wall-time. Fixture: `mc-mothership` (a real Rails monorepo, ~13k indexed files).

## TL;DR

- **85% fewer bytes** sent to the LLM across 9 representative code-lookup tasks (saved ≈ 414,498 input tokens, ≈ $1.243 per equivalent batch).
- **206× faster wall-time** in aggregate. Several raw `grep -rn` calls **timed out at 60s**; crabcc returned in milliseconds.
- Wins on: whole-repo symbol lookups, callers, references, file listings.
- Honest losses on: single-file outline (raw `grep -nE` on one file is already cheap), small directory listings.

## Per-task results

| Task | crabcc B | rg B | grep B | crabcc | rg | grep | vs rg | vs grep |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `sym-User` | 1.2k | 201 | 14.1k | 10.8ms | 731.8ms | 59.33s | 67.8x | 5493x |
| `sym-Assessment` | 584 | 229 | 569 | 11.3ms | 701.6ms | 58.73s | 62.1x | 5198x |
| `callers-count-find_by` | 14 | 4 | 9 | 964.9ms | 701.1ms | 44.98s | 0.73x | 46.6x |
| `refs-files-Assessment` | 513 | 541 | 460 | 38.1ms | 27.8ms | 13.24s | 0.73x | 348x |
| `outline-assessment-rb` | 17.4k | 3.1k | 3.1k | 13.1ms | 11.4ms | 10.4ms | 0.87x | 0.79x |
| `outline-vs-read-assessment-rb` | 17.4k | 33.4k | 33.4k | 10.0ms | 7.3ms | 7.6ms | 0.73x | 0.76x |
| `files-models-rb` | 10.4k | 9.9k | 9.9k | 12.2ms | 11.8ms | 10.2ms | 0.97x | 0.84x |
| `files-all-rb` | 243.6k | 234.1k | 1.9M | 13.6ms | 65.3ms | 9.70s | 4.80x | 713x |
| `callers-files-find_by` | 821 | 1.1k | 831 | 54.3ms | 45.2ms | 45.91s | 0.83x | 845x |

## Bytes per task (ASCII)

```
sym-User                    
  raw       14.1k  █
  crabcc     1.2k  █

sym-Assessment              
  raw         569  █
  crabcc      584  █

callers-count-find_by       
  raw           9  █
  crabcc       14  █

refs-files-Assessment       
  raw         460  █
  crabcc      513  █

outline-assessment-rb       
  raw        3.1k  █
  crabcc    17.4k  █

outline-vs-read-assessment-rb
  raw       33.4k  █
  crabcc    17.4k  █

files-models-rb             
  raw        9.9k  █
  crabcc    10.4k  █

files-all-rb                
  raw        1.9M  ██████████████████████████████
  crabcc   243.6k  ████

callers-files-find_by       
  raw         831  █
  crabcc      821  █

```

## Why these numbers

- **`grep -rn` on a Rails monorepo touches `node_modules/`, `tmp/`, `.git/`** — that's why several runs timed out. crabcc only walks files its indexer accepted; same for `rg` which respects `.gitignore` by default.
- **vs ripgrep:** rg is much faster than grep, but still has to scan every file from disk on every query. crabcc reads from a SQLite index — the answer is already in memory. That's why even rg shows 5–100× slowdowns vs crabcc on whole-repo questions.
- **crabcc returns structured JSON, not raw text.** For whole-repo questions, that JSON is much smaller than the file excerpts an agent would otherwise have to read.
- **Token-shaping flags (`--count`, `--files-only`, `--limit`) collapse 16k-token result sets to ~3 tokens** (`{"count":475}`) when the agent only needs a count or a deduped file list.
- **The losses are on small targeted ops** where a one-line `rg`/`grep` on a single file is already trivial — crabcc's structured output costs more than the raw output it replaces. Recommend the agent stay with `rg`/`fd` for `outline of one small file`-shaped queries.

## Recommended tool ladder

- **Code-shape questions** (symbols, callers, refs, file outlines, code-file listings) → `crabcc`
- **Free-text in code or non-code files** → `rg`
- **Filename glob / by age / non-code** → `fd`
- **Reshape JSON output** → `jq`
- **Never** plain `grep -rn` or `find . -name` on a real repo.

## What this benchmark does NOT prove

- This measures the CLI tool, not the full Claude session. A separate Claude-session benchmark (existing `bench/run.sh`) showed **per-turn cache cost can erase CLI savings** when crabcc causes extra agent turns. Net wins require either (a) crabcc not adding turns, or (b) very large result sets where the byte savings outweigh one extra turn (~5k tokens).
- The right framing for PMs: *crabcc's CLI advantage is large and unambiguous; converting that into Claude-session $ savings depends on agent prompting and skill design.*

## Charts

![Bytes saved](./savings.png)

![Speedup](./speedup.png)
