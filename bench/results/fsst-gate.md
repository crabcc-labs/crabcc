# FSST v2.0.0-alpha release gate

Bench data: see `bench/results/compress-20260429T233446Z.json` (gitignored).
Run on: 20260429T233446Z, host darwin/arm64, fixture `/Users/peter.lodri/workspace/mc-mothership`.

| Criterion | Threshold | Measured | Pass? |
|---|---|---|---|
| p99 single-row decode (in-process) | <1 ms | 0.000 ms | PASS |
| Signature compression ratio (signature_column) | >=1.4x | 1.86x | PASS |
| Indexing throughput regression | <10% | -15.2% | PASS |
| Test suite | zero regressions | n/a (run separately, see CI artifact) | n/a |

## Raw numbers

- FSST off: index 14017 ms, db 10,346,496 B
- FSST on:  index 11888 ms, db 9,158,656 B, rebuild 252 ms
- Bulk SQL throughput: 35.35 MB/s (5000 rows)

## Decision

PASS - recommend cutting v2.0.0-alpha.1.
