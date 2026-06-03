use std::fs;
use std::path::Path;

use crate::audit::model::SessionFile;
use crate::audit::tokens::estimate_tokens;

pub fn parse_session(path: &Path) -> anyhow::Result<SessionFile> {
    // Read the file contents
    let contents = fs::read_to_string(path)?;

    // Split into lines
    let lines: Vec<&str> = contents.lines().collect();

    // Count messages (lines)
    let messages = lines.len();

    // Calculate total tokens
    let total_tokens = lines.iter().map(|line| estimate_tokens(line)).sum();

    // Get project name (parent directory name)
    let project = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_string();

    Ok(SessionFile {
        path: path.display().to_string(),
        project,
        messages,
        total_tokens,
    })
}
