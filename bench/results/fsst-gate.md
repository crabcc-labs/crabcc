# FSST v2.0.0-alpha release gate

Bench data: see `bench/results/compress-20260429T232413Z.json` (gitignored).
Run on: 20260429T232413Z, host darwin/arm64, fixture `/Users/peter.lodri/workspace/mc-mothership`.

| Criterion | Threshold | Measured | Pass? |
|---|---|---|---|
| p99 single-row decode | <1 ms | 32.513 ms | FAIL |
| DB size reduction (signatures) | >=1.4x | 1.00x | FAIL |
| Indexing throughput regression | <10% | -22.9% | PASS |
| Test suite | zero regressions | n/a (run separately, see CI artifact) | n/a |

## Raw numbers

- FSST off: index 16956 ms, db 10,346,496 B
- FSST on:  index 13069 ms, db 10,346,496 B, rebuild 687 ms
- Bulk SQL throughput: 36.38 MB/s (5000 rows)

## Decision

INSPECT - see failing rows above; do not cut tag yet.
