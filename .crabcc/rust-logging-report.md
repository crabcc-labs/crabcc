# Rust logging audit — crabcc

Generated: 2026-04-30 · issue #90 · [tracing](https://github.com/tokio-rs/tracing)

## Highest-impact wins (Phase 1 — Cargo.toml)

| Check | Status |
|---|---|
| `tracing` adopted | ✅ workspace dep |
| `tracing-subscriber` adopted | ✅ workspace dep, features: env-filter, fmt, json |
| `tracing-appender` (non-blocking writer) | ✅ workspace dep + used in `crabcc-cli/src/telemetry.rs` |
| `tracing-opentelemetry` + `opentelemetry-otlp` | ❌ **MISSING** — issue #90 target, not yet wired |
| `tracing-log` bridge | N/A — no `log` crate in workspace |
| Mixed framework smell (slog/fern/env_logger) | ✅ clean — none present |
| `flate2` / `zlib-ng` | N/A — neither present |
| `regex` / `Mutex<Regex>` risk | ✅ clean — no `regex` crate |

**Highest-impact action:** wire `tracing-opentelemetry` + `opentelemetry-otlp` into `telemetry.rs` and point at the rotel OTLP endpoint (issue #86). The subscriber stack is already built; only the exporter layer is missing.

## Findings by cluster

### Library noise (Agent A) — severity: medium

`eprintln!` present in library crates. These bypass the structured tracing pipeline and can't be filtered, sampled, or exported.

| File | Disposition |
|---|---|
| `crates/crabcc-core/src/watch.rs` | Library — replace with `tracing::warn!` / `tracing::error!` |
| `crates/crabcc-core/src/graph.rs` | Library — replace with `tracing::debug!` |
| `crates/crabcc-core/src/extract.rs` | Library — replace with `tracing::warn!` |
| `crates/crabcc-memory/src/palace.rs` | Library — replace with `tracing::warn!` |
| `crates/crabcc-memory/src/backend/mod.rs` | Library — replace with `tracing::debug!` |
| `crates/crabcc-viz/src/lib.rs` | Library — replace with `tracing::info!` |

FP exclusions applied: `crabcc-cli/src/{main,compress_cmd,backup,agent,doctor,status,install,memory,go,agent_guard}.rs` — CLI user-facing output is legitimate `eprintln!`. `crabcc-memory-bench/src/main.rs` — bench binary, acceptable.

### Hot-path discipline (Agent B) — severity: low

- No `Mutex<Regex>` found ✅
- `format!` hits are all helper function names (`format_pair`, `format_text`, etc.), not inline format-then-log antipatterns ✅
- No `to_string()` in span fields found ✅

**Clean.** matthieum's hot-path rules are satisfied.

### Init-time hygiene (Agent C) — severity: low

- No `lazy_static!` ✅
- No `OnceLock` / `OnceCell` ✅
- `tracing_subscriber::*::init()` not in `crabcc-cli/src/main.rs` or `ccc.rs` directly — delegated to `crabcc-cli/src/telemetry.rs` (acceptable pattern; confirm `telemetry::init()` is called before any `tracing::*` macros in both binaries).
- `tracing_appender::non_blocking` used in `telemetry.rs` ✅

**No lazy first-call init jitter issues found.** Minor: verify `telemetry::init()` is the first call in both `main()` functions.

### Framework mix & OTLP wiring (Agent D) — severity: high

- No competing global subscribers (env_logger, slog, fern) ✅
- `non_blocking` writer confirmed ✅
- **Missing:** `tracing-opentelemetry` exporter. Issue #90 requires OTLP export to the rotel collector (issue #86). The `telemetry.rs` file has the subscriber stack but no OTLP layer.
- No `tracing::trace!` or `tracing::debug!` inside hot loops found — no non-blocking urgency beyond what's already wired.

## Skipped clusters

None skipped — all four clusters ran.

## CI fix (co-located)

Root cause of CI failure and local pre-commit ICE:
`cargo-features = ["profile-rustflags"]` in `Cargo.toml` requires nightly Cargo.
Stable Cargo (used in CI via `dtolnay/rust-toolchain@stable`) rejects the manifest entirely.
Additionally, `-Z polonius` + `-Z randomize-layout` in `[profile.dev]` triggered a `libc 0.2.186` ICE
in `rustc 1.97.0-nightly`, blocking every local commit.

**Fix applied:**
- Removed `cargo-features = ["profile-rustflags"]` from `Cargo.toml`
- Removed all `-Z` nightly flags from profile definitions
- Moved nightly extras as opt-in comments to `.cargo/config.toml`

## Score: **needs work**

| Cluster | Score |
|---|---|
| Library noise | 🟡 medium — eprintln! in 6 library files |
| Hot-path discipline | 🟢 good |
| Init-time hygiene | 🟢 good |
| Framework mix & OTLP | 🔴 high — OTLP exporter missing (issue #90) |

**Top 3 actions:**
1. `crates/crabcc-core/src/watch.rs` — replace `eprintln!` with `tracing::warn!`
2. Wire `tracing-opentelemetry` + `opentelemetry-otlp` in `telemetry.rs` (issue #90)
3. `crates/crabcc-memory/src/palace.rs` — replace `eprintln!` with `tracing::warn!`
