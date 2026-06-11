//! Read-only repo navigation tools shared by the Planner and the 3 Coders.
//!
//! Each tool shells out to the installed `crabcc` CLI (with cwd = the target
//! repo) or reads a file directly. They never write. If `crabcc` is missing or
//! exits non-zero, the tool returns a `ToolError` string rather than panicking
//! — the agent loop surfaces it to the model as a tool result.

use std::path::{Path, PathBuf};
use std::process::Command;

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::Deserialize;
use serde_json::json;

/// Cap on `read_file` output so a single tool call can't blow the context
/// window: whichever of ~400 lines / 60KB is hit first truncates the rest.
const MAX_FILE_LINES: usize = 400;
const MAX_FILE_BYTES: usize = 60 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("crabcc not found on PATH; install it or run from a repo where it is available")]
    CrabccNotFound,
    #[error("crabcc exited with status {code}: {stderr}")]
    CrabccFailed { code: i32, stderr: String },
    #[error("path escapes the repo root: {0}")]
    PathEscape(String),
    #[error("io error: {0}")]
    Io(String),
}

/// Run `crabcc <args...>` with cwd = `repo`, returning stdout on success.
fn run_crabcc(repo: &Path, args: &[&str]) -> Result<String, ToolError> {
    let output = Command::new("crabcc")
        .args(args)
        .current_dir(repo)
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ToolError::CrabccNotFound
            } else {
                ToolError::Io(e.to_string())
            }
        })?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(ToolError::CrabccFailed {
            code: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Resolve a caller-supplied relative path against the repo root, rejecting any
/// path that escapes it (`..`, absolute). Read-only tools must not be coaxed
/// into reading outside the target repo.
fn resolve_in_repo(repo: &Path, rel: &str) -> Result<PathBuf, ToolError> {
    let joined = repo.join(rel);
    let canon_repo = repo
        .canonicalize()
        .map_err(|e| ToolError::Io(e.to_string()))?;
    let canon = joined
        .canonicalize()
        .map_err(|e| ToolError::Io(e.to_string()))?;
    if !canon.starts_with(&canon_repo) {
        return Err(ToolError::PathEscape(rel.to_string()));
    }
    Ok(canon)
}

// ---- crabcc_sym ----------------------------------------------------------

#[derive(Deserialize)]
pub struct SymArgs {
    pub name: String,
}

#[derive(Clone)]
pub struct CrabccSym {
    pub repo: PathBuf,
}

impl Tool for CrabccSym {
    const NAME: &'static str = "crabcc_sym";
    type Error = ToolError;
    type Args = SymArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Find where a symbol (function, type, method) is defined in the repo. \
                Returns the definition location(s) and signature."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "The symbol name to look up" }
                },
                "required": ["name"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        run_crabcc(&self.repo, &["lookup", "sym", &args.name])
    }
}

// ---- crabcc_refs ---------------------------------------------------------

#[derive(Deserialize)]
pub struct RefsArgs {
    pub name: String,
}

#[derive(Clone)]
pub struct CrabccRefs {
    pub repo: PathBuf,
}

impl Tool for CrabccRefs {
    const NAME: &'static str = "crabcc_refs";
    type Error = ToolError;
    type Args = RefsArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Find all references / call sites of a symbol across the repo. \
                Use this to gauge the blast radius of changing it."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "The symbol name to find references to" }
                },
                "required": ["name"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        run_crabcc(&self.repo, &["lookup", "refs", &args.name])
    }
}

// ---- crabcc_outline ------------------------------------------------------

#[derive(Deserialize)]
pub struct OutlineArgs {
    pub path: String,
}

#[derive(Clone)]
pub struct CrabccOutline {
    pub repo: PathBuf,
}

impl Tool for CrabccOutline {
    const NAME: &'static str = "crabcc_outline";
    type Error = ToolError;
    type Args = OutlineArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Outline a file's top-level structure (its symbols) without reading the \
                whole file. Prefer this before read_file on a large file."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Repo-relative path to the file" }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        run_crabcc(&self.repo, &["lookup", "outline", &args.path])
    }
}

// ---- read_file -----------------------------------------------------------

#[derive(Deserialize)]
pub struct ReadFileArgs {
    pub path: String,
}

#[derive(Clone)]
pub struct ReadFile {
    pub repo: PathBuf,
}

impl Tool for ReadFile {
    const NAME: &'static str = "read_file";
    type Error = ToolError;
    type Args = ReadFileArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: format!(
                "Read a file from the repo (repo-relative path). Truncated to the first \
                 {MAX_FILE_LINES} lines / {} KB. Read-only.",
                MAX_FILE_BYTES / 1024
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Repo-relative path to the file" }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let path = resolve_in_repo(&self.repo, &args.path)?;
        let bytes = std::fs::read(&path).map_err(|e| ToolError::Io(e.to_string()))?;
        // Byte cap first (cheap), then line cap on the lossy-decoded text.
        let capped = &bytes[..bytes.len().min(MAX_FILE_BYTES)];
        let text = String::from_utf8_lossy(capped);
        let mut out = String::new();
        for (i, line) in text.lines().enumerate() {
            if i >= MAX_FILE_LINES {
                out.push_str("... [truncated]\n");
                break;
            }
            out.push_str(line);
            out.push('\n');
        }
        Ok(out)
    }
}
