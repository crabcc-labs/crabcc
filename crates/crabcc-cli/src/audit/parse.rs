use std::fs;
use std::path::Path;

use crate::audit::model::SessionFile;
use crate::audit::tokens::estimate_tokens;

pub fn parse_session(path: &Path) -> anyhow::Result<SessionFile> {
    let contents = fs::read_to_string(path)?;
    let messages = contents.lines().count();
    let total_tokens = contents.lines().map(|line| estimate_tokens(line)).sum();
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
