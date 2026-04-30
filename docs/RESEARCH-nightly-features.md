# Nightly-feature triage for crabcc

> Companion to `docs/RESEARCH-graph-prompt.md`. This document is the
> opinionated answer to "which nightly Rust features should crabcc
> adopt, and how do we sandbox the toolchain risk?".

## Toolchain strategy

**Don't pin the workspace to nightly.** Stable users — by far the
majority of contributors and the entire CI matrix as currently
configured — should keep working unchanged. Sandbox each nightly
feature behind a cargo feature flag on the *narrowest* crate that
benefits.

```toml
# crates/crabcc-memory/Cargo.toml
[features]
default = []
simd-cosine = []  # requires nightly rustc; opt-in
```

Inside the crate:

```rust
// crates/crabcc-memory/src/lib.rs
#![cfg_attr(feature = "simd-cosine", feature(portable_simd))]
```

The workspace stays 100% stable. Only `crabcc-memory` lights up the
nightly path, and only when explicitly built with
`cargo build --features simd-cosine` (which fails on stable, by design).

## Crate boundary

| Crate | Stability stance |
|---|---|
| `crabcc-core` | **Strictly stable.** AST extractor + index + symbol store. MSRV-sensitive — every dependent crate ships through here. Never gain a nightly feature gate. |
| `crabcc-mcp` | **Strictly stable.** The most-deployed surface (every Claude Code user runs this over stdio). Stable-only, no exceptions. |
| `crabcc-cli` | **Strictly stable.** Aggregates the rest; can't be more permissive than its deps. |
| `crabcc-memory` | **Sandbox crate for nightly experiments.** Currently lights up `portable_simd` behind `simd-cosine`. Future nightly-feature trials land here first. |

## CI matrix (proposed)

Three rows in `.github/workflows/ci.yml`:

| Row | Toolchain | Features | Required? |
|---|---|---|---|
| stable | `rust-toolchain: stable` | default | yes |
| nightly+simd | `rust-toolchain: nightly` | `--features simd-cosine` | allowed-failure initially, required once the prototype proves itself |
| msrv | `rust-toolchain: 1.86` | default | yes |

The `rust-version = "1.86"` pin in workspace `Cargo.toml` (added in this
PR) feeds the MSRV row. Bumping rules: only when a public dep forces
it (currently `rusqlite 0.31` + `fsst-rs 0.5` push the floor to 1.86).
Don't bump as a "what does the latest edition support?" exercise.

## Bench harness

`crates/crabcc-memory/src/backend/mod.rs::cosine_perf_smoke` is the
minimal in-tree harness. Marked `#[ignore]` so the regular `cargo
test` run skips it; invoke with:

```bash
cargo +nightly test --features simd-cosine \
    -p crabcc-memory backend::tests::cosine_perf_smoke -- --ignored --nocapture
```

Output: scalar wall-time, SIMD wall-time, speedup multiplier on a
representative 384-d × 1000-row workload. Use this to decide whether
the nightly dependency is worth the carrying cost. If the speedup is
< 2× on real hardware, drop the feature.

## Adoption checklist (in priority order)

| # | Feature | Verdict | Notes |
|---|---|---|---|
| 1 | [`portable_simd`](https://github.com/rust-lang/rust/issues/86656) | **Adopt** behind `simd-cosine`. | Direct hit on the brute-force cosine path. Wired up in this PR. |
| 2 | [`iter_array_chunks`](https://github.com/rust-lang/rust/issues/100450) | Skip. | We use `slice::chunks_exact` (stable since 1.0) inside the SIMD loop — same perf, one fewer feature gate. |
| 3 | `bumpalo::collections::Vec` (stable!) | Adopt before reaching for `allocator_api`. | The per-file tree-sitter walker copies symbol records into `Vec<Symbol>` before flushing to SQLite. Bumpalo would amortise allocations across the file. Stable, no nightly needed. |
| 4 | [`allocator_api`](https://github.com/rust-lang/rust/issues/32838) + `Vec<T, A>` | Defer until profiling proves bumpalo isn't enough. | Profile first. |
| 5 | [`try_blocks`](https://github.com/rust-lang/rust/issues/31436) | Adopt opportunistically inside `crabcc-mcp::handle`. | Small ergonomic win; trivial to revert. |
| 6 | [`gen` blocks](https://github.com/rust-lang/rust/issues/117078) | Defer. | Only justified once profiling shows eager `Vec` materialisation hurting MCP latency on huge repos. |
| 7 | [`box_into_inner`](https://github.com/rust-lang/rust/issues/80437) | Skip. | The SQLite query path's `Drawer` Box round-trips aren't a measurable hot path. |
| 8 | [`iter_intersperse`](https://github.com/rust-lang/rust/issues/79524) | Use the `itertools::intersperse` crate on stable. | One stable dep solves it. |
| 9 | [`iter_collect_into`](https://github.com/rust-lang/rust/issues/94780) | Defer. | Tiny saving; not load-bearing. |
| 10 | [`generic_const_exprs`](https://github.com/rust-lang/rust/issues/76560) | Skip. | Plain `min_const_generics` covers our `Simd<f32, LANES>` and similar use-cases. |

## Honest summary

The honest answer for crabcc as of April 2026: **probably one nightly
feature (`portable_simd` for cosine) is worth the carrying cost**, and
even that should stay feature-gated behind `simd-cosine`. Everything
else is either already stable, has a stable workaround that gets 80%
of the benefit, or isn't load-bearing enough on crabcc's actual hot
paths to justify pinning a toolchain.

## What to do next

1. Land the `simd-cosine` prototype + bench (this PR).
2. Run `cosine_perf_smoke` on the actual hot-path workload size you
   care about (1k drawers? 10k? 100k?). Decide whether the speedup
   justifies the ongoing nightly-toolchain dance.
3. If the speedup is real, add the `nightly+simd` CI row as
   allowed-failure first, then promote to required.
4. If the speedup isn't there, delete the feature and the nightly
   gate. The scalar path is the canonical fallback and is always
   available.
5. Revisit this doc whenever a new "is X nightly feature ready?" pull
   crosses your desk. Default answer: no, unless the table above
   already says yes.
