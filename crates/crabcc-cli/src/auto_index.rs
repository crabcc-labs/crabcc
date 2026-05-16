//! Lazy first-time indexing for read-side commands.
//!
//! Calling `crabcc outline foo.rs` on a project that has never been
//! indexed used to return an empty list. Now we detect that the store
//! holds zero files, print a one-line stderr warning, and run a full
//! index + FTS rebuild before letting the read query proceed.
//!
//! Opt out with `CRABCC_NO_AUTO_INDEX=1` (handy for scripts that wrap
//! `crabcc index` themselves).

use crate::root_resolver::ResolvedRoot;
use anyhow::Result;
use crabcc_core::store::Store;

/// If the store has no indexed files, run a full index + FTS rebuild.
/// Cheap no-op once the index is populated (single SELECT COUNT).
pub fn ensure_indexed(resolved: &ResolvedRoot, store: &Store) -> Result<()> {
    if std::env::var_os("CRABCC_NO_AUTO_INDEX").is_some() {
        return Ok(());
    }
    if !is_empty(store)? {
        return Ok(());
    }
    let source_dir = resolved.source_dir.as_path();
    let fts_dir = resolved.fts_dir();
    eprintln!(
        "warning: project not indexed yet — indexing now ({})",
        resolved.display_origin()
    );
    let started = std::time::Instant::now();
    let stats = crabcc_core::index::full_index(source_dir, store)?;
    store.mark_schema_v4_built()?;
    if let Ok(fts) = crabcc_core::fts::Fts::open(&fts_dir) {
        let _ = fts.rebuild(store);
    }
    eprintln!(
        "warning: indexed {} files ({} symbols) in {:.2}s",
        stats.files_indexed,
        stats.symbols,
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

fn is_empty(store: &Store) -> Result<bool> {
    Ok(store.list_files()?.is_empty())
}
