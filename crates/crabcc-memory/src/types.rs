//! Public data types: `Drawer`, `Wing`, `Query`, `Session`, `DrawerHit`, etc.
//! All `Serialize + Deserialize` for MCP and CLI JSON I/O.

use serde::{Deserialize, Serialize};

pub type DrawerId = i64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wing {
    pub id: i64,
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrawerInsert {
    pub wing: String,
    pub room: Option<String>,
    pub source_id: String,
    pub body: String,
    pub embedding: Vec<f32>,
    /// Optional session bucket for "what did I capture from this terminal /
    /// MCP invocation". Populated from `$TERM_SESSION_ID` or MCP tool args.
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Drawer {
    pub id: DrawerId,
    pub wing: String,
    pub room: Option<String>,
    pub source_id: String,
    pub body: String,
    pub sha256: String,
    pub created_at: i64,
    #[serde(default)]
    pub session_id: Option<String>,
}

/// Session record — created on first drawer insert from a given session_id,
/// or explicitly via `Palace::start_session`. `cwd` / `git_branch` / `git_sha`
/// captured at session start.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub started_at: i64,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub git_sha: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Query {
    pub embedding: Vec<f32>,
    pub limit: usize,
    pub wing: Option<String>,
    pub room: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrawerHit {
    pub id: DrawerId,
    pub score: f32,
    pub source_id: String,
    pub body: String,
    pub wing: String,
    pub room: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    pub hits: Vec<DrawerHit>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct GetResult {
    pub drawers: Vec<Drawer>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeleteSel {
    ById(Vec<DrawerId>),
    BySource(String),
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HealthStatus {
    Ok,
    Degraded,
    Down,
}
