pub mod aggregate;
pub mod discover;
pub mod model;
pub mod parse;
pub mod render;
pub mod run;
pub mod tokens;
pub mod waste;

#[derive(clap::Subcommand)]
pub enum AuditOp {
    /// Scan Claude session logs for token waste.
    Scan {
        /// Dir to scan (default ~/.claude/projects).
        path: Option<std::path::PathBuf>,
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },
}

pub fn run(op: &AuditOp) -> anyhow::Result<()> {
    match op {
        AuditOp::Scan { path, json } => crate::audit::run::run_audit(path.as_deref(), *json),
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod audit_tests;
