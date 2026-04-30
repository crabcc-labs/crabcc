# MIGRATION-RFC — `tracing` adoption for crabcc

> Status: **draft**
> Tracking: [issue #90](https://github.com/peterlodri-sec/crabcc/issues/90)
> Companion skill: [`SKILL.md`](./SKILL.md) (validates this RFC's
> end-state)
> Source theses (full context in issue #90):
> - [Arko 2025 — Rust keeps parsing those logs faster](https://andre.arko.net/2025/03/28/rust-keeps-parsing-those-logs-faster/)
> - [Ochagavía / matthieum — Low-latency logging in Rust](https://ochagavia.nl/blog/low-latency-logging-in-rust/)
> - [tokio-rs/tracing](https://github.com/tokio-rs/tracing)

## Goals

1. **Workspace-wide** structured instrumentation via `tracing`.
2. **Hot-path safe** — no allocs, no formatting, no blocking I/O at the
   call site.
3. **OTLP exporter** feature-gated, terminating at the rotel panel from #86.
4. **No regressions** — agent latency ≤ +2 %, binary size ≤ +5 %, token
   cost from `crabcc track` flat.

## Non-goals (v1)

- Custom subscriber implementing matthieum's `constructor` + `AtomicU32`
  activation gate. We adopt `tracing-subscriber`'s `EnvFilter` first;
  re-evaluate only if profiling shows the filter is hot.
- Per-statement runtime toggles on `/live`. Slot for a follow-up.
- On-disk log format migration. The existing `.crabcc/logs` schema is
  preserved; tracing emits in addition to (not instead of) it during the
  migration.

## End-state (what the workspace looks like after this RFC lands)

### Workspace `Cargo.toml`

```toml
[workspace.dependencies]
tracing            = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }
tracing-appender   = "0.2"
# OTLP — feature-gated per crate via `otlp` feature.
tracing-opentelemetry = { version = "0.27", optional = true }
opentelemetry        = { version = "0.26", optional = true }
opentelemetry-otlp   = { version = "0.26", features = ["grpc-tonic"], optional = true }
opentelemetry_sdk    = { version = "0.26", features = ["rt-tokio"], optional = true }
# log → tracing bridge so existing log::* calls in deps still surface.
tracing-log = "0.2"
```

### `crabcc-cli/src/telemetry.rs` (new module)

```rust
//! Telemetry init. Called exactly once from main(), early.
use std::io;
use tracing_subscriber::{fmt, EnvFilter, prelude::*};

pub struct TelemetryGuard {
    // Holds the non-blocking writer worker; drop on shutdown to flush.
    _writer_guard: tracing_appender::non_blocking::WorkerGuard,
    #[cfg(feature = "otlp")]
    _otlp_provider: opentelemetry_sdk::trace::TracerProvider,
}

pub fn init() -> TelemetryGuard {
    // 1. Non-blocking stderr — matthieum rule: hot path returns in ns.
    let (writer, guard) = tracing_appender::non_blocking(io::stderr());

    // 2. EnvFilter: RUST_LOG=crabcc=info,crabcc_core=debug etc. Compile-time
    //    static metadata stays in tracing's macros; filter is the only
    //    runtime decision per call.
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("crabcc=info,warn"));

    let fmt_layer = fmt::layer()
        .with_writer(writer)
        .with_target(true)
        .with_ansi(io::IsTerminal::is_terminal(&io::stderr()));

    let registry = tracing_subscriber::registry().with(filter).with(fmt_layer);

    #[cfg(feature = "otlp")]
    let (registry, _otlp_provider) = {
        let endpoint = std::env::var("CRABCC_OTLP")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "http://127.0.0.1:4317".to_string());
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint)
            .build()
            .expect("OTLP exporter init");
        let provider = opentelemetry_sdk::trace::TracerProvider::builder()
            .with_batch_exporter(exporter, opentelemetry_sdk::runtime::Tokio)
            .build();
        let tracer = provider.tracer("crabcc");
        (registry.with(tracing_opentelemetry::layer().with_tracer(tracer)), provider)
    };

    registry.init();
    tracing_log::LogTracer::init().ok();   // log::* → tracing.

    TelemetryGuard {
        _writer_guard: guard,
        #[cfg(feature = "otlp")]
        _otlp_provider,
    }
}
```

### Hot-path call sites

```rust
// before
eprintln!("indexing {} files", n);

// after — fields, not formatting; subscriber serializes.
tracing::info!(file_count = n, "indexing");
```

```rust
// before — hot loop with per-iter format
for hit in hits {
    eprintln!("hit at {}:{}", hit.file, hit.line);
}

// after — span the loop, log fields per iter
let _span = tracing::trace_span!("emit_hits", count = hits.len()).entered();
for hit in &hits {
    tracing::trace!(file = %hit.file, line = hit.line, "hit");
}
```

### MCP tool spans

```rust
#[tracing::instrument(level = "info", skip_all, fields(tool = "memory.search"))]
pub async fn memory_search(req: SearchReq) -> Result<SearchRsp> { … }
```

This auto-emits a span per call, with fields, that rotel turns into an OTLP
span attribute set without any extra code.

## Phased migration

### Phase 1 — wiring (1 PR, no behavior change)

- Add the deps above to workspace `Cargo.toml`.
- Add `crabcc-cli/src/telemetry.rs` and call `telemetry::init()` from
  `main()` *before* the existing logging setup. No call sites touched yet.
- Existing `eprintln!` / `log::*` continue to work; `tracing-log` bridges
  any `log::*` deps.
- Acceptance: `RUST_LOG=crabcc=debug crabcc index .` produces formatted
  tracing output AND existing eprintln output. Bench gates green.

### Phase 2 — library noise (per-crate PRs)

For each library crate, in this order:

1. `crabcc-memory` (highest log volume → biggest immediate win).
2. `crabcc-core` (largest surface area).
3. `crabcc-mcp` (most user-facing — adds `#[instrument]` per tool).
4. `crabcc-viz` (server routes; spans help debug `/live`).

Per-crate steps:

1. Run `/rust-logging-audit` on the workspace, scope to the crate.
2. Replace findings in the "library noise" cluster:
   - `eprintln!` → `tracing::{warn,error}!` with field-style args.
   - `println!` in lib code → `tracing::info!` (or remove if accidental).
   - `log::*` macros → `tracing::*` (no functional change; bridge keeps deps working).
3. Add `#[tracing::instrument(level = "info", skip_all, fields(…))]` to:
   - `crabcc-mcp` tool entry points.
   - `crabcc-viz` HTTP route handlers.
   - `crabcc-memory` `remember`, `search`, `forget`, `mine`.
   - `crabcc-core` index-time hot fns: `Store::open`, `extract`, hot graph
     walks. **Only** when callers count > 1 (use
     `crabcc callers <fn> --count`).

Acceptance per PR: skill returns no `severity: high` findings for the crate
under audit. Bench gates green. `cargo bloat --release --crates` shows
≤ 1 % delta per PR.

### Phase 3 — hot-path discipline (Arko + matthieum)

Audit findings from skill agents B and C:

- Replace `Mutex<Regex>` with per-thread `thread_local!` `Regex` or
  `OnceLock<Regex>` initialized in `main`.
- Move every `lazy_static!` / `OnceLock` whose initializer compiles a
  regex / opens a file / dials a network into eager init in
  `crabcc-cli`'s `main()` *before* `telemetry::init()` returns.
- For any `format!` that lives inside a `tracing::*` argument, replace
  with field-style args.
- Switch `flate2` to `flate2 = { version = "…", features = ["zlib-ng"] }`
  if `bench-compress` shows a win (Arko's x86 +15 % observation).

Acceptance: `task bench-compress` clears existing gate; `criterion`
benches in `crabcc-memory-bench/` show no regression > 2 %.

### Phase 4 — OTLP wiring + `/live` integration (depends on #86)

- Land issue #86 (rotel `/live` panel) first — out of this RFC's scope.
- Enable `otlp` feature in `crabcc-cli` release builds.
- `CRABCC_OTLP=http://127.0.0.1:4317` in dev launches local rotel; the
  `/live` panel renders incoming spans.
- Document the env var and the rotel sub-process supervision model in
  `README.md`.

Acceptance: an agent run produces ≥ 1 span per MCP tool invocation
visible in `/live`'s rotel panel within 2 s of completion.

## Risk / rollback

- **All four phases are individually revertible.** Phase 1 is a pure add;
  Phase 2's `eprintln!` → `tracing::*` is a one-line revert per call site;
  Phase 3 is gated by benches; Phase 4 is feature-gated.
- **Binary size**: `tracing-opentelemetry` + `opentelemetry-*` brings ~7
  transitive deps. The `otlp` feature is OFF by default; release builds
  decide per profile.
- **MSRV**: `tracing` 0.1.x is fine on 1.86. `tracing-opentelemetry` 0.27
  also works on 1.86 — verify in CI's MSRV row before Phase 1 PR merges.
- **First-call jitter**: addressed by Phase 3 (init-time evaluation of
  every `lazy_static!`).

## Validation matrix (per PR)

| Check                                          | Phase | Tool                         |
|------------------------------------------------|------:|------------------------------|
| `task fmt-check` + `task lint`                 | all   | existing CI                  |
| `task ci`                                      | all   | existing CI                  |
| `cargo bloat --release --crates`               | 2 / 3 | new advisory job             |
| `task bench-compress`                          | 3     | existing perf gate           |
| `task memory-bench`                            | 2 / 3 | existing R@5 gate            |
| `/rust-logging-audit .` returns 0 highs        | 2 / 3 | this skill                   |
| `criterion` baseline ≤ +2 %                    | 3     | `crabcc-memory-bench`        |
| Span visible in `/live` rotel panel            | 4     | manual + integration test    |

## Open questions

1. Do we want `tracing-tree` for indented dev-time output, or stick with
   `fmt`? Indented output reads better for nested spans (e.g. a tool call
   that spawns memory ops). Tentatively yes; decide in Phase 1 PR review.
2. Is the rotel sub-process supervision in #86 going to expose a control
   socket for `/live` to introspect span flow? If not, we'll add a
   dedicated `/api/spans/recent` endpoint in `crabcc-viz` that taps the
   non-blocking writer for the panel. Decide jointly with #86.
3. Should `CRABCC_AUTO_MEMORY=1` capture spans (in addition to drawer
   notes) when set? Probably yes — the matthieum rule "register
   everything at init time" suggests we always emit spans and let the
   subscriber decide. Decide in Phase 4.

## Appendix — why not `slog` / `log` / `defmt`?

- `log` lacks structured fields and spans; `tracing-log` lets us bridge
  legacy callers without forcing the framework choice.
- `slog` has fields and is fast, but the ecosystem (esp. OTLP exporters)
  has consolidated around `tracing` since ~2022.
- `defmt` is for embedded / no_std; not applicable here.
- Custom (matthieum-style `constructor` + `AtomicU32`) is faster but
  bespoke. We choose `tracing` for ecosystem leverage; revisit only if
  profiling justifies the maintenance cost.
