//! Runtime helpers shared by `crabcc serve` and `crabcc agent`:
//! ensuring the repo is fully initialized, and laying out
//! `~/.crabcc/bin/` so spawned agent processes find a consistent set
//! of binaries on PATH regardless of the user's shell config.
//!
//! Why both surfaces want this:
//!   - `crabcc serve` should land in a "ready to render" state, so the
//!     live dashboard's bootstrap call has real numbers (not zeros).
//!   - `crabcc agent` spawns Claude Code, whose `Bash` tool calls each
//!     start a fresh shell. Shell aliases don't survive across calls;
//!     symlinks on PATH do.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

/// Where we install our shim/symlink directory. Goes onto the agent's
/// PATH ahead of `~/.cargo/bin` and `~/.local/bin` so a `crabcc`
/// invocation from inside an agent's `Bash` tool call resolves to the
/// version we want, not whatever's in the user's primary install.
pub const CRABCC_BIN_DIR: &str = ".crabcc/bin";

/// Bootstrap result — what `ensure_initialized` actually did. Useful
/// for the launch banner and for tests that assert side-effect shape
/// without re-implementing `go::init`'s contract here.
#[derive(Debug, Default)]
pub struct InitOutcome {
    pub created_index: bool,
    pub created_graph: bool,
    pub created_memory: bool,
    pub files: usize,
    pub symbols: usize,
    pub graph_edges: usize,
    pub drawers: usize,
}

/// Locate the user's home dir. Mirrors agent.rs / install.rs lookups.
pub fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME not set; cannot resolve ~/.crabcc/"))
}

/// Bring `<root>/.crabcc/{index.db,graph.json,memory.db}` into a
/// consistent "ready" state. Cheap on already-initialized repos
/// (`refresh` does an mtime sweep); does a full index on cold ones.
///
/// Mirrors what `crabcc go` does — same crate (`crabcc-core::index`),
/// same call shape, so future bumps to the init contract apply to
/// both surfaces.
pub fn ensure_initialized(root: &Path) -> Result<InitOutcome> {
    let mut out = InitOutcome::default();

    let crabcc_dir = root.join(".crabcc");
    std::fs::create_dir_all(&crabcc_dir)
        .with_context(|| format!("create {}", crabcc_dir.display()))?;

    let db = crabcc_dir.join("index.db");
    out.created_index = !db.exists();
    let store = crabcc_core::store::Store::open(&db).context("open .crabcc/index.db")?;
    if out.created_index {
        let stats = crabcc_core::index::full_index(root, &store)?;
        out.files = stats.files_indexed;
        out.symbols = stats.symbols;
    } else {
        let _ = crabcc_core::index::refresh(root, &store)?;
        out.files = store.list_files().map(|v| v.len()).unwrap_or(0);
        out.symbols = store.iter_all_symbols().map(|v| v.len()).unwrap_or(0);
    }

    // Graph sidecar — rebuild on cold init, leave the cached version
    // alone otherwise (refresh doesn't invalidate edges; rebuilding
    // each time would be wasted work on warm runs).
    let graph_path = crabcc_dir.join("graph.json");
    if !graph_path.exists() {
        out.created_graph = true;
        let g = crabcc_core::graph::CallGraph::build(&store, root)?;
        g.save(&graph_path)?;
        out.graph_edges = g.edge_count;
    } else if let Ok(g) = crabcc_core::graph::CallGraph::load(&graph_path) {
        out.graph_edges = g.edge_count;
    }

    // Memory db — touch via Palace::open. Idempotent; bootstraps the
    // schema if the file is missing.
    let memory_path = crabcc_dir.join("memory.db");
    out.created_memory = !memory_path.exists();
    if let Ok(palace) = crabcc_memory::palace::Palace::open(root) {
        out.drawers = palace.count().unwrap_or(0);
    }

    // Service-discovery sidecar (issue #143) — write the resolved URLs +
    // reachability snapshot to `.crabcc/services.json` so other processes
    // on the same host (telegram bot, jobs-worker, future Apple Container
    // sidecar) can read what we resolved without invoking the discovery
    // module themselves. Best-effort: don't fail serve init if the write
    // fails (could be readonly fs in a container).
    let services_path = crabcc_dir.join("services.json");
    let report = crabcc_core::service_discovery::discover_all();
    if let Ok(s) = serde_json::to_string_pretty(&report) {
        let _ = std::fs::write(&services_path, s);
    }

    Ok(out)
}

/// Set up `~/.crabcc/bin/` with symlinks for the binaries we want
/// agent subprocesses to find on PATH. Today we install:
///
///   - `crabcc` -> the running crabcc binary (so the agent's Bash tool
///     calls hit the same version that started `crabcc serve` /
///     `crabcc agent`).
///   - `cc` -> a wrapper script that just execs `crabcc "$@"`.
///
/// The wrapper for `cc` is a written script (not a symlink) because
/// `cc` is reserved on most Unixes for the C compiler — symlinking
/// it would shadow the C toolchain. Putting a hand-written script
/// makes the override explicit and easier to remove (`rm`) later.
///
/// Idempotent: symlinks pointing at the right target are left alone.
/// Returns the bin-dir path so callers can append it to `PATH`.
pub fn ensure_bin_dir(home: &Path) -> Result<PathBuf> {
    let bin_dir = home.join(CRABCC_BIN_DIR);
    std::fs::create_dir_all(&bin_dir).with_context(|| format!("create {}", bin_dir.display()))?;

    // Resolve the running `crabcc` binary. `current_exe` is reliable
    // on macOS + Linux (which is what we ship for). On Windows it
    // might be a TrampolineExe; we don't ship Windows agents today.
    if let Ok(self_exe) = std::env::current_exe() {
        symlink_idempotent(&self_exe, &bin_dir.join("crabcc"))?;
    }

    // `cc` shim — hand-written, executable. Re-write if missing or stale.
    let cc_path = bin_dir.join("cc");
    let want = "#!/bin/sh\n\
                # Generated by crabcc; safe to delete. \
                Re-created on next `crabcc serve` or `crabcc agent` start.\n\
                exec crabcc \"$@\"\n";
    let needs_write = std::fs::read_to_string(&cc_path)
        .map(|s| s != want)
        .unwrap_or(true);
    if needs_write {
        std::fs::write(&cc_path, want).with_context(|| format!("write {}", cc_path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut p = std::fs::metadata(&cc_path)?.permissions();
            p.set_mode(0o755);
            std::fs::set_permissions(&cc_path, p)?;
        }
    }

    Ok(bin_dir)
}

#[cfg(unix)]
fn symlink_idempotent(target: &Path, link: &Path) -> Result<()> {
    if let Ok(existing) = std::fs::read_link(link) {
        if existing == target {
            return Ok(());
        }
    }
    if link.exists() || std::fs::symlink_metadata(link).is_ok() {
        std::fs::remove_file(link).ok();
    }
    std::os::unix::fs::symlink(target, link)
        .with_context(|| format!("symlink {} -> {}", link.display(), target.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn symlink_idempotent(_target: &Path, _link: &Path) -> Result<()> {
    // Windows symlinks need elevated perms; skip silently — the agent
    // PATH extension still helps on Windows because `~/.cargo/bin/`
    // already has the right binary.
    Ok(())
}

/// Compose the PATH string we want spawned agent processes to inherit.
/// Prepend `~/.crabcc/bin`, `~/.cargo/bin`, `~/.local/bin` to whatever
/// the parent already has. We don't *replace* PATH — just bias the
/// front of it — so the agent still finds `git`, `gh`, `jq`, `rg`,
/// etc. that the user had on PATH already.
pub fn agent_path(home: &Path) -> String {
    let mut out: Vec<PathBuf> = Vec::new();
    out.push(home.join(CRABCC_BIN_DIR));
    out.push(home.join(".cargo").join("bin"));
    out.push(home.join(".local").join("bin"));
    if let Ok(existing) = std::env::var("PATH") {
        for p in std::env::split_paths(&existing) {
            if !out.iter().any(|x| x == &p) {
                out.push(p);
            }
        }
    }
    std::env::join_paths(out)
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_bin_dir_creates_dir_and_cc_shim() {
        let home = tempfile::tempdir().unwrap();
        let dir = ensure_bin_dir(home.path()).unwrap();
        assert!(dir.starts_with(home.path()));
        assert!(dir.exists());
        let cc = dir.join("cc");
        assert!(cc.exists(), "cc shim must be installed");
        let body = std::fs::read_to_string(&cc).unwrap();
        assert!(
            body.contains("exec crabcc"),
            "cc shim should exec crabcc: {body}"
        );
    }

    #[test]
    fn ensure_bin_dir_is_idempotent() {
        let home = tempfile::tempdir().unwrap();
        let dir1 = ensure_bin_dir(home.path()).unwrap();
        let cc1_mtime = std::fs::metadata(dir1.join("cc"))
            .unwrap()
            .modified()
            .unwrap();
        // Re-run; the cc shim file should not be re-written when the
        // body matches what we want — preserves mtime so users + tools
        // don't see a "changed" signal on every server start.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let dir2 = ensure_bin_dir(home.path()).unwrap();
        let cc2_mtime = std::fs::metadata(dir2.join("cc"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(dir1, dir2);
        assert_eq!(cc1_mtime, cc2_mtime, "idempotent run must not bump mtime");
    }

    #[test]
    fn agent_path_prepends_crabcc_bin_dir() {
        let home = tempfile::tempdir().unwrap();
        let path = agent_path(home.path());
        let first_segments: Vec<_> = path
            .split(if cfg!(windows) { ';' } else { ':' })
            .take(3)
            .collect();
        assert!(
            first_segments[0].ends_with(CRABCC_BIN_DIR),
            "first PATH entry must be ~/.crabcc/bin: {first_segments:?}"
        );
        assert!(first_segments[1].ends_with(".cargo/bin"));
        assert!(first_segments[2].ends_with(".local/bin"));
    }

    #[test]
    fn ensure_initialized_creates_index_and_graph() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.rs"),
            "pub fn outer() { inner(); }\npub fn inner() {}\n",
        )
        .unwrap();
        let outcome = ensure_initialized(dir.path()).unwrap();
        assert!(outcome.created_index);
        assert!(outcome.created_graph);
        assert!(dir.path().join(".crabcc/index.db").exists());
        assert!(dir.path().join(".crabcc/graph.json").exists());
        // Issue #143 — services.json should land alongside the other
        // sidecars on serve init. Best-effort write; assert it parses
        // back to a DiscoveryReport.
        let services_path = dir.path().join(".crabcc/services.json");
        assert!(services_path.exists(), "services.json should be written");
        let body = std::fs::read_to_string(&services_path).unwrap();
        let report: crabcc_core::service_discovery::DiscoveryReport =
            serde_json::from_str(&body).expect("services.json must parse");
        assert!(
            !report.services.is_empty(),
            "services.json must list at least one service"
        );
    }
}
