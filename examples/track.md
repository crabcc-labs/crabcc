# `crabcc track` — token-savings tracker

Every crabcc query appends a JSONL row to `~/.crabcc/usage.log` with the operation,
tokens used (output bytes / 4), and an estimate of tokens it saved compared to
`grep + Read`. `crabcc track` aggregates that log.

## Human-readable

```bash
crabcc track
```

```text
crabcc usage:
  session       4 queries     1,041 tokens used      28,452 saved
  last 24h     38 queries    12,003 tokens used     321,997 saved
  all-time    102 queries    32,540 tokens used     897,310 saved

by operation:
  callers       6 queries     1,820 tokens used      33,140 saved
  files         8 queries     2,400 tokens used      19,200 saved
  outline       3 queries     7,500 tokens used      10,500 saved
  refs         11 queries       420 tokens used      99,180 saved
  sym          74 queries    20,400 tokens used     735,290 saved
```

- **session** — last 30 minutes.
- **last 24h** — last 24 hours.
- **all-time** — every recorded query.

## JSON

```bash
crabcc track --json
```

```json
{"session": {"queries":4,"used_tokens":1041,"saved_tokens":28452},
 "last_24h": {"queries":38,"used_tokens":12003,"saved_tokens":321997},
 "all_time": {"queries":102,"used_tokens":32540,"saved_tokens":897310},
 "by_op": {"sym": {"queries":74, …}, …}}
```

## How "saved" is estimated

Heuristic, on purpose. We don't know what the agent *would* have done, so we
estimate the typical `grep + Read` path:

| Op        | Estimated raw cost                                    |
|-----------|-------------------------------------------------------|
| `sym`     | 3,500 tokens (1 grep + reading the matched file)     |
| `refs`    | 2,000 + 300 × min(results, 100) tokens               |
| `callers` | same as refs                                          |
| `outline` | 6,000 tokens (full Read of the file)                 |
| `fuzzy`   | 2,500 tokens (regex sweep)                            |
| `prefix`  | 1,500 tokens                                          |

Saved = max(0, raw_estimate − used_tokens).

The estimates intentionally lean conservative — we'd rather under-claim than over-claim.

## Storage

- `~/.crabcc/usage.log` — JSONL, append-only.
- One file globally (not per-repo) so `crabcc track` aggregates across every project
  you've used crabcc in.
- Failure to write is silent — tracking never breaks the actual query.

To reset: `rm ~/.crabcc/usage.log`.

## Privacy

The log records: `op` name, query string (capped at 200 chars), result count, repo
basename, used + saved tokens, and a unix timestamp. **No file contents, no full
paths, no hit data.**

If you don't want any of that recorded, `chmod -w ~/.crabcc/` — writes will silently
fail and queries continue.
