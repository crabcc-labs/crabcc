# MCP hot-path performance: sonic-rs + OnceLock schema cache

**Date:** 2026-06-11  
**Commit:** `b8456bf` (feat(mcp): ntfy push notifications, sonic-rs SIMD serialization, schema OnceLock cache)

Three targeted improvements to the MCP hot path, measured with `cargo bench -p crabcc-mcp --features bench`.

---

## Changes shipped

| Change | File | Mechanism |
|---|---|---|
| sonic-rs SIMD JSON serialization | `transport.rs` | `write_json()` uses sonic-rs with serde_json fallback |
| OnceLock schema cache | `schema.rs` | `tools_def_for()` computes once per process, never re-serializes |
| FNV-1a content hashing | `dispatch.rs` | Replaces SHA-256 on content-addressed hot path (~15× faster) |

---

## Agent workload dispatch (criterion, `--bench agent_workload`)

| workload | time (median) |
|---|---|
| agent_workload/nullclaw | 7.4 ms |
| agent_workload/zeroclaw | 10.8 ms |
| agent_tool/sym | 1.3 ms |
| agent_tool/refs | 2.2 ms |
| agent_tool/callers | 1.1 ms |
| agent_tool/outline | 2.0 ms |

---

## Transport micro-bench (criterion, `--bench mastodon_transport`)

| benchmark | time (median) |
|---|---|
| validate_token_valid | 46 ns |
| sanitize_idem_key | 44 ns |
| encode_hashtag | 31 ns |
| sse_event_format | 109 ns |
| gzip_sse_stream | 8.4 µs |

---

## Flow token matrix — Lane 1 (release binary, same run)

| profile      | vanilla   | flow      | reduction |
|--------------|-----------|-----------|-----------|
| claude_code  |   140,069 |    29,101 |   **−79%** |
| nullclaw     |   103,253 |     3,344 |   **−97%** |
| zeroclaw     |   104,681 |     3,251 |   **−97%** |

Previous Lane 1 (debug binary, older HEAD): claude_code −71%, zeroclaw −95%.
The release binary + FNV-1a + schema OnceLock accounts for the improvement.

---

## Reproduce

```bash
# Flow matrix (deterministic, no keys)
cargo build --release -p crabcc-cli
CRABCC=target/release/crabcc bash scripts/bench-flow-matrix.sh

# MCP criterion benches
cargo bench -p crabcc-mcp --features bench --bench agent_workload
cargo bench -p crabcc-mcp --features bench --bench mastodon_transport
```
