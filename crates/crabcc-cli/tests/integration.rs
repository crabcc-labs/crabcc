//! Single integration-test binary for the `crabcc` CLI.
//!
//! Each `tests/*.rs` file in a Cargo project becomes its own binary
//! with its own link step. macOS's `ld64` is single-threaded — the per-
//! binary link cost is the dominant compile-time factor for tests on
//! this workspace. Consolidating them here cuts ~10-15s off a cold
//! `cargo nextest run` and proportionally less on warm rebuilds.
//!
//! Add new integration test files under `tests/integration/` and one
//! `mod <name>;` line below.

// `tests/<name>.rs` is the root of its binary; `mod foo;` looks in the
// same directory, not a subdirectory. `#[path = ...]` points at the
// real files under `tests/integration/`.
#[path = "integration/affected.rs"]
mod affected;
#[path = "integration/agent_dry_run.rs"]
mod agent_dry_run;
#[path = "integration/auto_index.rs"]
mod auto_index;
#[path = "integration/cross_module_e2e.rs"]
mod cross_module_e2e;
#[path = "integration/e2e_walkdir.rs"]
mod e2e_walkdir;
#[path = "integration/graph_v4.rs"]
mod graph_v4;
#[path = "integration/shell_context_e2e.rs"]
mod shell_context_e2e;
#[path = "integration/shell_rewrite_e2e.rs"]
mod shell_rewrite_e2e;
