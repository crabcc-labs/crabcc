//! `crabcc go` — one-shot init + Claude launch.
//!
//! Single zero-arg command that brings a repo to a "ready to talk to
//! Claude" state in one breath:
//!
//!   1. Detect: is this repo crabcc-initialized (`.crabcc/index.db`)?
//!   2. Index: `full_index` if fresh, `refresh` if existing.
//!   3. Sidecar: rebuild Tantivy fuzzy/prefix search.
//!   4. Sidecar: rebuild the call-graph.
//!   5. Memory: open (or create) `.crabcc/memory.db`.
//!   6. Report: print a one-line status block.
//!   7. Hand off to `claude --effort max --append-system-prompt
//!      <AGENTS.md> --no-chrome` so the LLM session lands with the
//!      crabcc primer + repo context already loaded.
//!
//! Idempotent: re-running on an already-initialized repo refreshes the
//! sidecars (cheap) and skips the full reindex.

use anyhow::{anyhow, Context, Result};
use crabcc_core::store::Store;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Outcome of the init pass — useful for tests + machine consumers.
/// Not serialized to JSON yet; callers print a human-readable summary.
#[derive(Debug, Default)]
pub struct GoReport {
    pub was_initialized: bool,
    pub files_indexed: usize,
    pub symbols: usize,
    pub edges: usize,
    pub graph_edges: usize,
    pub drawer_count: usize,
}

pub fn run(root: &Path, db: &Path) -> Result<()> {
    println!("crabcc go :: {}", root.display());

    let report = init(root, db)?;
    print_summary(&report);

    let prompt = read_agents_prompt(root);
    spawn_claude(&prompt)?;
    Ok(())
}

/// Test-friendly entry point — runs every step EXCEPT spawning Claude.
/// Returns the populated `GoReport` so callers can assert against it.
pub fn init(root: &Path, db: &Path) -> Result<GoReport> {
    // Bootstrap the .crabcc directory before anything tries to open
    // a sqlite file inside it. Idempotent (mkdir -p semantics).
    if let Some(parent) = db.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }

    let mut report = GoReport {
        was_initialized: db.exists(),
        ..Default::default()
    };

    // Step 1 — open the symbol store. Creates `.crabcc/index.db` if
    // missing (Store::open runs the schema bootstrap unconditionally).
    let store = Store::open(db).context("open .crabcc/index.db")?;

    // Step 2 — index. Distinction: a freshly-created store has zero
    // files indexed → full pass; an existing store gets a cheap refresh.
    if !report.was_initialized {
        let stats = crabcc_core::index::full_index(root, &store)?;
        report.files_indexed = stats.files_indexed;
        report.symbols = stats.symbols;
        report.edges = stats.edges;
    } else {
        // refresh updates by mtime — much cheaper on warm runs. The stats
        // here are deltas, not totals; we sample current totals via
        // accessors that already exist on Store (avoid a fresh count(*)
        // round-trip per metric on a warm DB).
        let _ = crabcc_core::index::refresh(root, &store)?;
        report.symbols = store
            .iter_all_symbols()
            .map(|v| v.len())
            .unwrap_or_default();
        report.edges = store.edge_count().map(|n| n as usize).unwrap_or_default();
        report.files_indexed = store.list_files().map(|v| v.len()).unwrap_or_default();
    }

    // Step 3 — call-graph sidecar.
    let graph_path = root.join(".crabcc").join("graph.json");
    let graph = crabcc_core::graph::CallGraph::build(&store, root)?;
    graph.save(&graph_path)?;
    report.graph_edges = graph.edge_count;

    // Step 4 — memory palace. Delegated to the crabcc-memory crate so
    // the same db semantics apply as `crabcc memory init`.
    let palace = crabcc_memory::Palace::open(root)?;
    report.drawer_count = palace.count().unwrap_or_default();

    Ok(report)
}

/// Print the report as a tidy block. Format intentionally stable so
/// scripts can grep for `symbols :` / `drawers :` etc.
pub fn print_summary(r: &GoReport) {
    println!(
        "  {} {}",
        if r.was_initialized { "↻" } else { "✚" },
        if r.was_initialized {
            "refreshed"
        } else {
            "initialized"
        }
    );
    println!("  ✓ files   : {}", r.files_indexed);
    println!("  ✓ symbols : {}", r.symbols);
    println!("  ✓ edges   : {}", r.edges);
    println!("  ✓ graph   : {} edges", r.graph_edges);
    println!("  ✓ drawers : {}", r.drawer_count);
}

/// Locate the `claude` binary. Preference order: `claude`, `claude-code`.
/// We don't pull in the `which` crate — a manual PATH walk is short and
/// keeps the dep tree flat.
fn find_claude() -> Option<PathBuf> {
    for name in ["claude", "claude-code"] {
        if let Ok(path_var) = std::env::var("PATH") {
            for dir in std::env::split_paths(&path_var) {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

fn spawn_claude(prompt: &str) -> Result<()> {
    let claude = find_claude().ok_or_else(|| {
        anyhow!(
            "`claude` CLI not on PATH; install Claude Code first \
             (https://claude.ai/code) and re-run `crabcc go`"
        )
    })?;

    println!(
        "  → launching {} (--effort max --no-chrome, prompt: {} chars)",
        claude.display(),
        prompt.len()
    );

    let status = Command::new(&claude)
        .arg("--effort")
        .arg("max")
        .arg("--append-system-prompt")
        .arg(prompt)
        .arg("--no-chrome")
        .status()
        .with_context(|| format!("spawn {}", claude.display()))?;

    if !status.success() {
        anyhow::bail!("claude exited with {status}");
    }
    Ok(())
}

/// Read `AGENTS.md` to use as the appended system prompt. Falls back to
/// a minimal hardcoded primer if the file isn't present so `go` still
/// works in repos that haven't adopted AGENTS.md yet.
fn read_agents_prompt(root: &Path) -> String {
    let path = root.join("AGENTS.md");
    if let Ok(text) = std::fs::read_to_string(&path) {
        return text;
    }
    fallback_prompt()
}

fn fallback_prompt() -> String {
    String::from(
        "This repository is crabcc-indexed. Prefer the following symbol-aware \
         lookups over plain grep/find:\n\
         - `crabcc sym <Name>`     — find a definition\n\
         - `crabcc refs <Name>`    — find references / call sites\n\
         - `crabcc callers <Name>` — find direct callers\n\
         - `crabcc outline <file>` — top-level symbols of a file\n\
         - `crabcc memory search \"<query>\"` — hybrid (BM25 + vector) recall \
         from the per-repo memory store at `.crabcc/memory.db`.\n\
         Run `crabcc info` for build provenance. The MCP server (`crabcc mcp`) \
         exposes the same surface as tool calls.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    use crate::test_support::ensure_test_crabcc_home;

    #[test]
    fn init_creates_crabcc_dir_and_dbs() {
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        let root = dir.path();
        let db = root.join(".crabcc").join("index.db");
        let report = init(root, &db).unwrap();
        assert!(!report.was_initialized);
        assert!(root.join(".crabcc").join("index.db").exists());
        let memory_path = crabcc_memory::resolve_db_path(root).unwrap();
        assert!(
            memory_path.exists(),
            "expected memory.db at {}",
            memory_path.display()
        );
        assert!(root.join(".crabcc").join("graph.json").exists());
    }

    #[test]
    fn init_is_idempotent() {
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        let root = dir.path();
        let db = root.join(".crabcc").join("index.db");
        let r1 = init(root, &db).unwrap();
        let r2 = init(root, &db).unwrap();
        assert!(!r1.was_initialized);
        assert!(r2.was_initialized);
    }

    #[test]
    fn init_indexes_a_simple_typescript_file() {
        // Drop a single .ts file in the tempdir, run init, expect the
        // symbol count to reflect the function we wrote.
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("hello.ts"),
            "export function hello(name: string) { return name; }\n",
        )
        .unwrap();
        let db = root.join(".crabcc").join("index.db");
        let report = init(root, &db).unwrap();
        assert!(report.files_indexed >= 1, "expected at least 1 file");
        assert!(report.symbols >= 1, "expected at least 1 symbol");
    }

    #[test]
    fn fallback_prompt_mentions_crabcc_subcommands() {
        let p = fallback_prompt();
        for needle in [
            "crabcc sym",
            "crabcc refs",
            "crabcc callers",
            "memory search",
        ] {
            assert!(
                p.contains(needle),
                "fallback prompt missing `{needle}`: {p}"
            );
        }
    }

    #[test]
    fn read_agents_prompt_prefers_file_when_present() {
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        let agents_path = dir.path().join("AGENTS.md");
        std::fs::write(&agents_path, "MARKER-12345").unwrap();
        let p = read_agents_prompt(dir.path());
        assert!(p.contains("MARKER-12345"));
    }

    #[test]
    fn read_agents_prompt_falls_back_when_absent() {
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        let p = read_agents_prompt(dir.path());
        assert!(p.contains("crabcc sym"));
    }

    #[test]
    fn print_summary_formats_initialized_state() {
        // Smoke: call print_summary with both was_initialized states and
        // confirm the function does not panic. Captures the formatting
        // path (no easy stdout-capture in stable Rust without crates).
        let r1 = GoReport {
            was_initialized: false,
            files_indexed: 5,
            symbols: 12,
            edges: 7,
            graph_edges: 7,
            drawer_count: 0,
        };
        let r2 = GoReport {
            was_initialized: true,
            ..r1
        };
        print_summary(&r1);
        print_summary(&r2);
    }

    #[test]
    fn find_claude_returns_none_with_empty_path() {
        // Temporarily blank PATH and verify find_claude declines.
        let orig = std::env::var("PATH").ok();
        // SAFETY: tests in this crate are not parallelized across env reads.
        unsafe {
            std::env::set_var("PATH", "");
        }
        let result = find_claude();
        if let Some(p) = orig {
            unsafe {
                std::env::set_var("PATH", p);
            }
        }
        assert!(result.is_none());
    }
}
