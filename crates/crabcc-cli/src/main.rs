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
    /// Show estimated tokens saved by crabcc usage (this session, 24h, all-time).
    Track {
        /// Emit JSON instead of human-readable output.
        #[arg(long)]
        json: bool,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    let root = cli.root.unwrap_or_else(|| std::env::current_dir().unwrap());
    let db = root.join(".crabcc").join("index.db");

    if cli.mcp {
        return crabcc_mcp::serve_stdio(&root);
    }

    std::fs::create_dir_all(db.parent().unwrap())?;
    let store = Store::open(&db)?;

    match cli.cmd.unwrap_or(Cmd::Index) {
        Cmd::Index => {
            let stats = crabcc_core::index::full_index(&root, &store)?;
            println!("{}", serde_json::to_string(&stats)?);
        }
        Cmd::Refresh => {
            let stats = crabcc_core::index::refresh(&root, &store)?;
            println!("{}", serde_json::to_string(&stats)?);
        }
        Cmd::Sym { name } => {
            let syms = query::find_symbol(&store, &name)?;
            let body = serde_json::to_string(&syms)?;
            crabcc_core::track::record("sym", &name, syms.len(), &repo_label(&root), body.len());
            println!("{body}");
        }
        Cmd::Refs { name } => {
            let hits = crabcc_core::query::find_refs(&store, &root, &name)?;
            let body = serde_json::to_string(&hits)?;
            crabcc_core::track::record("refs", &name, hits.len(), &repo_label(&root), body.len());
            println!("{body}");
        }
        Cmd::Callers { name } => {
            let hits = crabcc_core::query::find_callers(&store, &root, &name)?;
            let body = serde_json::to_string(&hits)?;
            crabcc_core::track::record("callers", &name, hits.len(), &repo_label(&root), body.len());
            println!("{body}");
        }
        Cmd::Outline { file } => {
            let key = file.to_string_lossy();
            let syms = crabcc_core::outline::outline(&store, &key)?;
            let body = serde_json::to_string(&syms)?;
            crabcc_core::track::record("outline", &key, syms.len(), &repo_label(&root), body.len());
            println!("{body}");
        }
        Cmd::Grep { pattern } => {
            // TODO: ripgrep `grep` crate, annotate hits with enclosing symbol.
            println!("{{\"status\":\"todo\",\"op\":\"grep\",\"pattern\":\"{pattern}\"}}");
        }
        Cmd::Track { json } => {
            let r = crabcc_core::track::report()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                print_track_human(&r);
            }
        }
    }
    Ok(())
}

fn repo_label(root: &PathBuf) -> String {
    root.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("?")
        .to_string()
}

fn print_track_human(r: &crabcc_core::track::Report) {
    fn line(label: &str, b: &crabcc_core::track::Bucket) {
        println!(
            "  {:<10}  {:>6} queries   {:>9} tokens used   {:>10} saved",
            label, b.queries, b.used_tokens, b.saved_tokens
        );
    }
    println!("crabcc usage:");
    line("session",  &r.session);
    line("last 24h", &r.last_24h);
    line("all-time", &r.all_time);
    if !r.by_op.is_empty() {
        println!("\nby operation:");
        for (op, b) in &r.by_op {
            line(op, b);
        }
    }
}
