//! `crabcc read <path>` — thin CLI wrapper.
//!
//! All compute lives in [`crabcc_memory::read`] so the MCP `read`
//! tool can call the same code path and return identical JSON. This
//! file's only job is to (a) parse args, (b) print the resulting
//! `serde_json::Value` to stdout, and (c) record into the
//! `crabcc track` ledger.

use anyhow::Result;
use crabcc_core::store::Store;
use crabcc_memory::read::{compute, ReadMode};
use std::path::{Path, PathBuf};

pub fn run(
    root: &Path,
    store: &Store,
    path: PathBuf,
    mode_raw: &str,
    session_id_arg: Option<String>,
    entropy_threshold: f64,
) -> Result<()> {
    let mode = ReadMode::parse(mode_raw)?;
    let session_id = effective_session_id(session_id_arg);
    let path_label = path.to_string_lossy().to_string();
    let payload = compute(root, store, path, mode, session_id, entropy_threshold)?;
    let body = payload.to_string();
    crabcc_core::track::record("read", &path_label, 1, &repo_label(root), body.len());
    println!("{body}");
    Ok(())
}

fn effective_session_id(arg: Option<String>) -> Option<String> {
    if let Some(id) = arg {
        if !id.trim().is_empty() {
            return Some(id);
        }
    }
    std::env::var("CRABCC_SESSION_ID")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

fn repo_label(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| root.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    static SESSION_ID_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_session_env() -> std::sync::MutexGuard<'static, ()> {
        SESSION_ID_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn effective_session_id_prefers_flag_over_env() {
        let _g = lock_session_env();
        std::env::set_var("CRABCC_SESSION_ID", "env-id");
        let got = effective_session_id(Some("flag-id".to_string()));
        std::env::remove_var("CRABCC_SESSION_ID");
        assert_eq!(got.as_deref(), Some("flag-id"));
    }

    #[test]
    fn effective_session_id_falls_back_to_env() {
        let _g = lock_session_env();
        std::env::set_var("CRABCC_SESSION_ID", "env-id");
        let got = effective_session_id(None);
        std::env::remove_var("CRABCC_SESSION_ID");
        assert_eq!(got.as_deref(), Some("env-id"));
    }

    #[test]
    fn effective_session_id_treats_blank_as_none() {
        let _g = lock_session_env();
        std::env::remove_var("CRABCC_SESSION_ID");
        assert!(effective_session_id(Some("   ".to_string())).is_none());
        assert!(effective_session_id(None).is_none());
    }
}
