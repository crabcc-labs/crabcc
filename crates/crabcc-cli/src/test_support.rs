//! Crate-wide test scaffolding. The only inhabitant today is
//! [`ensure_test_crabcc_home`], which pins `$CRABCC_HOME` to a
//! single tempdir for the test process.
//!
//! Why one shared OnceLock and not per-module: tests across
//! `backup` / `go` / `memory` modules touch the same env var
//! (#479's memory.db relocation moved both the memory store and,
//! transitively, the backup root under `$CRABCC_HOME`). With
//! per-module OnceLocks they each pinned to a different tempdir
//! and raced; one shared OnceLock means every test in this crate
//! sees the same value, no race possible.

use std::path::PathBuf;
use std::sync::OnceLock;

/// Pin `$CRABCC_HOME` to a single tempdir for the entire test
/// process. Idempotent and thread-safe — the first caller leaks
/// a `TempDir` so the directory survives for the process lifetime,
/// subsequent callers re-set the env var to that same path.
///
/// Re-asserting the env var on every call is necessary because
/// other test modules may legitimately mutate it (the `backup`
/// tests historically swap roots between snapshots), and the
/// re-assertion brings everyone back to the shared pin.
pub(crate) fn ensure_test_crabcc_home() -> PathBuf {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    let path = HOME.get_or_init(|| {
        let d = tempfile::tempdir().expect("test crabcc-home tempdir");
        let p = d.path().to_path_buf();
        std::mem::forget(d);
        p
    });
    std::env::set_var("CRABCC_HOME", path);
    path.clone()
}
