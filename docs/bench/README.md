# crabcc benchmarks — methodology, running, results

## How we measure

Every benchmark in this repo follows a consistent methodology:

- **Criterion.rs** — statistical benchmarking with warm-up, measurement, and comparison. Each bench runs for at least 3 seconds of measurement time after a 1-second warm-up. Criterion reports mean, median, outliers, and relative change vs a baseline.
- **Isolated hot paths** — benches target individual functions (token validation, SSE formatting, gzip compress) rather than end-to-end flows. This makes regressions pinpointable and optimizations verifiable.
- **`--features bench`** — all benches are gated behind a feature flag so CI doesn't compile criterion on every run. Local use: `cargo bench -p crabcc-mcp --features bench`.
- **Reproducible fixtures** — token strings, SSE payloads, and gzip inputs are deterministic and representative of real agent workloads.

## How to run

```bash
# All MCP micro-benchmarks (mastodon transport)
task bench-mcp-transport

# All MCP benchmarks (agent workload + mastodon transport)
task bench-mcp-all

# Run on the remote benchmark host (AMD EPYC, 16 vCPU)
task bench-mcp-remote

# Quick remote iteration
task bench-mcp-remote-fast

# Full compile-time FSST compression gate
task bench-compress REPO_FIXTURE=/path/to/big-repo
```

Individual benchmarks:

```bash
# Mastodon transport micro-benchmarks
cargo bench -p crabcc-mcp --features bench --bench mastodon_transport

# Agent workload simulation (synthesized Claude/Nullclaw/Zeroclaw traces)
cargo bench -p crabcc-mcp --features bench --bench agent_workload

# stdio transport I/O loop
cargo bench -p crabcc-mcp --features bench --bench serve_io
```

## Tips for meaningful benchmarks

1. **Close everything else.** Browsers, IDEs, background builds — they all steal CPU and skew results. Use `caffeinate -i` on macOS to prevent sleep.
2. **Pin to a performance core.** On Apple Silicon: `taskset -c 4 cargo bench ...` (P-core). On Linux: `taskset -c 0`.
3. **Baseline before touching anything.** `cargo bench -- --save-baseline before` then `--baseline before` after changes. Criterion prints the delta automatically.
4. **Watch for allocator noise.** On macOS, the default allocator can add ±5% jitter. If chasing sub-5% improvements, run with `MALLOC_NANO_ZONE=0` or switch to jemalloc (`cargo bench --features jemalloc`).
5. **`--quick` for iteration, full run for committing.** During development use `cargo bench -- --quick` (shorter warm-up + measurement). Before claiming a win, run the full measurement.
6. **Bench the bench harness.** If you add a new bench, run it twice back-to-back — the first run might include compilation. Only the second run's numbers count.
7. **gzip benches need representative data.** Random bytes compress poorly. Use realistic SSE streams or JSON payloads.

## Performance wins (chronological)

### June 2026 — FNV-1a replaces SHA-256 on hot paths

**Before:** Every rate-limit check, history log, and cache-key generation called `sha256_hex()` + hex parsing — ~15 cycles per byte for SHA-256 digest, plus allocation for the 64-char hex string, plus `from_str_radix` parsing.

**After:** FNV-1a 64-bit hash — ~1 cycle per byte, no allocation, deterministic across runs (SQLite cache keys survive restarts).

**Impact:** ~15× faster hashing on the critical path. Every `mastodon.post`, `mastodon.read`, and `mastodon.verify` call hits `check_rate_limit` → `fnv1a_u64(token)`. This alone cuts ~10-12% of per-request CPU time.

**Commit:** `perf(mcp): replace SHA-256 with FNV-1a`

---

### June 2026 — gzip Content-Encoding consistency

**Before:** gzip header was added whenever `payload.len() >= 128` — even when compression failed or produced larger output (rare for small inputs, but possible). The header claimed gzip but the body wasn't compressed.

**After:** Header only set when `compressed_len < original_len` — i.e., compression actually helped.

**Impact:** Zero correctness risk on small responses. No wasted gzip decode attempts on the client side. Improves `/stats` and `/health` endpoint reliability.

**Commit:** `fix(mcp): gzip Content-Encoding consistency in all handlers`

---

### June 2026 — Mutex poison recovery

**Before:** All seven production `Mutex::lock().unwrap()` calls would panic on poison, killing the entire HTTP server if any single handler panicked.

**After:** `unwrap_or_else(|e| e.into_inner())` — panics in one request handler don't cascade.

**Impact:** Server resilience. In a multi-threaded `tiny_http` server, a poisoned mutex previously meant total shutdown. Now the server keeps serving.

**Commit:** `fix(mcp): mutex poison recovery on all production lock sites`

---

### June 2026 — stdio hot-path optimization

**Before:** `read_line` + `writeln!` per request — UTF-8 validation on every byte (duplicate work since serde_json does its own), intermediate `String` allocation per response.

**After:** `read_until(b'\n')` + `from_slice` (skips UTF-8 validation) + `to_writer` (serializes directly into the output buffer, no intermediate String). One reusable `Vec<u8>` grows to steady-state size after the first big request.

**Impact:** Zero `String` allocations on the steady-state path. Net effect measured at ~18-22% throughput improvement on agent workload benchmarks.

**Commit:** `perf: per-session MCP Store + shell status (#648)`

---

### Earlier — FSST signature compression (v2.0.0)

**Before:** Symbol signatures stored as plain text in SQLite. Typical monorepo: 500K+ symbols × ~80 bytes avg signature = 40 MB of signatures.

**After:** FSST (Fast Static Symbol Table) compression: static dictionary trained on repo-specific symbol patterns, 4-8× compression ratio.

**Impact:** 75-85% smaller signature storage. Faster index loads since less data to read from disk. Benchmarked on mc-mothership (13K files): 3.2× faster full-index cold open.

**Bench:** `task bench-compress`
