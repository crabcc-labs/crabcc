use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::process::Command;

/// Shared config: path to the crabcc binary and repo root.
#[derive(Clone)]
pub struct CrabccConfig {
    pub binary: PathBuf,
    pub root: PathBuf,
}

impl Default for CrabccConfig {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("crabcc"),
            root: std::env::current_dir().unwrap_or_default(),
        }
    }
}

// ── Tool 1: SymLookup ────────────────────────────────────────────────────────
// Runs: crabcc --root <root> lookup sym <name>

#[derive(Serialize, Deserialize)]
pub struct SymInput {
    pub name: String,
}

#[derive(Serialize, Deserialize)]
pub struct SymOutput {
    pub symbols: serde_json::Value,
}

pub struct SymLookup(pub CrabccConfig);

impl Tool for SymLookup {
    const NAME: &'static str = "crabcc_sym";
    type Error = std::io::Error;
    type Args = SymInput;
    type Output = SymOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Look up a symbol by name in the indexed codebase. Returns file, line, kind, and signature.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Symbol name to look up" }
                },
                "required": ["name"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let out = Command::new(&self.0.binary)
            .args(["--root", &self.0.root.to_string_lossy(), "lookup", "sym", &args.name])
            .output()
            .await?;
        let symbols: serde_json::Value = serde_json::from_slice(&out.stdout)
            .unwrap_or(serde_json::Value::Array(vec![]));
        Ok(SymOutput { symbols })
    }
}

// ── Tool 2: RefsLookup ───────────────────────────────────────────────────────
// Runs: crabcc --root <root> lookup refs <name>

#[derive(Serialize, Deserialize)]
pub struct RefsInput {
    pub name: String,
}

#[derive(Serialize, Deserialize)]
pub struct RefsOutput {
    pub refs: serde_json::Value,
}

pub struct RefsLookup(pub CrabccConfig);

impl Tool for RefsLookup {
    const NAME: &'static str = "crabcc_refs";
    type Error = std::io::Error;
    type Args = RefsInput;
    type Output = RefsOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Find all references to a name in the codebase. Returns file, line, snippet.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Symbol name to find references for" }
                },
                "required": ["name"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let out = Command::new(&self.0.binary)
            .args(["--root", &self.0.root.to_string_lossy(), "lookup", "refs", &args.name])
            .output()
            .await?;
        let refs: serde_json::Value = serde_json::from_slice(&out.stdout)
            .unwrap_or(serde_json::Value::Array(vec![]));
        Ok(RefsOutput { refs })
    }
}

// ── Tool 3: CallerLookup ─────────────────────────────────────────────────────
// Runs: crabcc --root <root> lookup callers <name>

#[derive(Serialize, Deserialize)]
pub struct CallersInput {
    pub name: String,
}

#[derive(Serialize, Deserialize)]
pub struct CallersOutput {
    pub callers: serde_json::Value,
}

pub struct CallerLookup(pub CrabccConfig);

impl Tool for CallerLookup {
    const NAME: &'static str = "crabcc_callers";
    type Error = std::io::Error;
    type Args = CallersInput;
    type Output = CallersOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Find all callers of a function in the codebase. Returns caller file, line, and enclosing function.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Function name to find callers of" }
                },
                "required": ["name"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let out = Command::new(&self.0.binary)
            .args(["--root", &self.0.root.to_string_lossy(), "lookup", "callers", &args.name])
            .output()
            .await?;
        let callers: serde_json::Value = serde_json::from_slice(&out.stdout)
            .unwrap_or(serde_json::Value::Array(vec![]));
        Ok(CallersOutput { callers })
    }
}
