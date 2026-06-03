use anyhow::Result;
use std::path::{Path, PathBuf};

pub fn find_session_logs(root: &Path) -> Result<Vec<PathBuf>> {
    let mut logs = Vec::new();

    if !root.exists() {
        return Ok(logs);
    }

    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() && entry.path().extension().is_some_and(|ext| ext == "jsonl")
        {
            logs.push(entry.into_path());
        }
    }

    Ok(logs)
}
