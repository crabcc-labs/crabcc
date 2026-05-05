//! Shared test scaffolding for `crabcc_memory`. Today the only
//! inhabitant is [`ensure_test_crabcc_home`], which pins
//! `$CRABCC_HOME` to a single tempdir for the whole test process so
//! `Palace::open` (and now `shell::record_shell`) tests don't fight
//! over the env var.
//!
//! One shared `OnceLock` per CRATE — `palace::tests::ensure_test_crabcc_home`
//! and `shell::tests::ensure_test_crabcc_home` previously had
//! separate locks and raced under cargo's parallel runner. Both
//! now route here.

use std::path::PathBuf;
use std::sync::OnceLock;

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
