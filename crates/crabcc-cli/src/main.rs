use anyhow::Result;
use clap::{Parser, Subcommand};
use crabcc_core::{query, store::Store};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "crabcc", version, about = "Symbol index for AI coding agents")]
struct Cli {
    /// Path to repo root (defaults to cwd).
    #[arg(long, global = true)]
    root: Option<PathBuf>,

    /// Run as MCP server over stdio instead of one-shot CLI.
    #[arg(long, global = true)]
    mcp: bool,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Build a fresh index for the repo.
    Index,
    /// Incremental reindex of changed files.
    Refresh,
    /// Find a symbol by name.
    Sym { name: String },
    /// Find references to a name.
    Refs { name: String },
    /// Find callers of a function.
    Callers { name: String },
    /// File outline (top-level symbols, no bodies).
    Outline { file: PathBuf },
    /// Symbol-aware grep wrapper.
    Grep { pattern: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    let root = cli.root.unwrap_or_else(|| std::env::current_dir().unwrap());
    let db = root.join(".crabcc").join("index.db");

    if cli.mcp {
        return crabcc_mcp::serve_stdio(&root).await;
    }

    std::fs::create_dir_all(db.parent().unwrap())?;
    let store = Store::open(&db)?;

    match cli.cmd.unwrap_or(Cmd::Index) {
        Cmd::Index => {
            let stats = crabcc_core::index::build_index(&root, &store)?;
            println!("{}", serde_json::to_string(&stats)?);
        }
        Cmd::Refresh => {
            // For v1, refresh == full rebuild. Hash-keyed incremental coming next.
            let stats = crabcc_core::index::build_index(&root, &store)?;
            println!("{}", serde_json::to_string(&stats)?);
        }
        Cmd::Sym { name } => {
            let syms = query::find_symbol(&store, &name)?;
            println!("{}", serde_json::to_string(&syms)?);
        }
        Cmd::Refs { name } => {
            // TODO: graph edges + ripgrep fallback.
            println!("{{\"status\":\"todo\",\"op\":\"refs\",\"name\":\"{name}\"}}");
        }
        Cmd::Callers { name } => {
            println!("{{\"status\":\"todo\",\"op\":\"callers\",\"name\":\"{name}\"}}");
        }
        Cmd::Outline { file } => {
            println!("{{\"status\":\"todo\",\"op\":\"outline\",\"file\":\"{}\"}}",
                     file.display());
        }
        Cmd::Grep { pattern } => {
            // TODO: ripgrep `grep` crate, annotate hits with enclosing symbol.
            println!("{{\"status\":\"todo\",\"op\":\"grep\",\"pattern\":\"{pattern}\"}}");
        }
    }
    Ok(())
}
