//! Test scaffolding shared across `crabcc_mcp` test modules
//! ([`crate::memory`] and the `tests` mod in `lib.rs`).
//!
//! [`ensure_test_crabcc_home`] pins `$CRABCC_HOME` to a single
//! tempdir for the whole test process. Without one shared
//! OnceLock, per-module pins would each create their own tempdir
//! and fight over the env var.

use std::path::PathBuf;
use std::sync::OnceLock;

pub(crate) fn ensure_test_crabcc_home() {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    let path = HOME.get_or_init(|| {
        let d = tempfile::tempdir().expect("test crabcc-home tempdir");
        let p = d.path().to_path_buf();
        std::mem::forget(d);
        p
    });
    std::env::set_var("CRABCC_HOME", path);
}
