use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use crabcc_core::{query, store::Store};
use std::path::{Path, PathBuf};

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
    Refs {
        name: String,
        #[command(flatten)]
        opts: ResultOpts,
    },
    /// Find callers of a function.
    Callers {
        name: String,
        #[command(flatten)]
        opts: ResultOpts,
    },
    /// File outline (top-level symbols, no bodies).
    Outline { file: PathBuf },
    /// List indexed files (replaces `ls -R` / `find -name`).
    Files {
        /// Restrict to paths starting with PREFIX.
        #[arg(long)]
        under: Option<String>,
        /// Restrict to a language: typescript, tsx, javascript, ruby.
        #[arg(long)]
        lang: Option<String>,
        /// Restrict to file extension (without leading dot).
        #[arg(long)]
        ext: Option<String>,
        /// Cap output. 0 means unlimited.
        #[arg(long, default_value_t = 0)]
        limit: usize,
    },
    /// Symbol-aware grep wrapper.
    Grep { pattern: String },
    /// Fuzzy symbol-name search (Levenshtein distance 2).
    Fuzzy {
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Prefix symbol-name search (case-insensitive).
    Prefix {
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Rebuild the Tantivy fuzzy/prefix sidecar from the current SQLite index.
    FtsRebuild,
    /// Show estimated tokens saved by crabcc usage (this session, 24h, all-time).
    Track {
        #[arg(long)]
        json: bool,
    },
    /// Watch the repo and auto-`refresh` on file changes (Ctrl-C to exit).
    Watch {
        /// Debounce window in milliseconds — events within this window
        /// collapse into one refresh. Lower = more responsive, higher = less
        /// thrash on burst events like `git checkout`.
        #[arg(long, default_value_t = 500)]
        debounce: u64,
    },
    /// Build the call-graph sidecar (.crabcc/graph.json).
    GraphBuild,
    /// Query the call-graph: who calls / what does this symbol call?
    Graph {
        name: String,
        /// Direction: 'callers' (default) walks upward; 'callees' walks downward.
        #[arg(long, default_value = "callers")]
        dir: String,
        /// BFS depth limit.
        #[arg(long, default_value_t = 2)]
        depth: usize,
    },
}

/// Shaping flags for refs/callers. `--files-only` and `--count` are
/// mutually exclusive output shapes; `--limit` modifies whichever shape
/// you picked (or the default hit-list shape).
#[derive(Args, Debug, Clone)]
struct ResultOpts {
    /// Cap result size at N. 0 = unlimited. Applies to hits or file list.
    #[arg(long, default_value_t = 0)]
    limit: usize,
    /// Emit deduped JSON array of file paths only — no line/col/snippet.
    #[arg(long, conflicts_with = "count")]
    files_only: bool,
    /// Emit `{"count": N}` only — no per-hit payload.
    #[arg(long)]
    count: bool,
}

impl ResultOpts {
    fn to_mode(&self) -> query::Mode {
        if self.count {
            query::Mode::Count
        } else if self.files_only {
            query::Mode::FilesOnly {
                limit: opt(self.limit),
            }
        } else {
            query::Mode::Hits {
                limit: opt(self.limit),
            }
        }
    }
}

fn opt(n: usize) -> Option<usize> {
    if n == 0 {
        None
    } else {
        Some(n)
    }
}

fn main() -> Result<()> {
    // Default to warn — tantivy emits INFO chatter on every commit otherwise.
    // Override with RUST_LOG=info for debugging.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
    // Closed-pipe (e.g. piping to `head`) should exit silently, not panic.
    reset_sigpipe();

    let cli = Cli::parse();
    let root = cli.root.unwrap_or_else(|| std::env::current_dir().unwrap());
    let db = root.join(".crabcc").join("index.db");

    if cli.mcp {
        return crabcc_mcp::serve_stdio(&root);
    }

    std::fs::create_dir_all(db.parent().unwrap())?;
    let store = Store::open(&db)?;
    let fts_dir = root.join(".crabcc").join("tantivy");

    match cli.cmd.unwrap_or(Cmd::Index) {
        Cmd::Index => {
            let stats = crabcc_core::index::full_index(&root, &store)?;
            // Rebuild Tantivy too — keep the fuzzy/prefix sidecar in lockstep
            // with a full reindex. (refresh deliberately does not.)
            if let Ok(fts) = crabcc_core::fts::Fts::open(&fts_dir) {
                let _ = fts.rebuild(&store);
            }
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
        Cmd::Refs { name, opts } => {
            let mode = opts.to_mode();
            let out = query::query_refs(&store, &root, &name, mode)?;
            let body = serde_json::to_string(&out)?;
            crabcc_core::track::record("refs", &name, out.count(), &repo_label(&root), body.len());
            println!("{body}");
        }
        Cmd::Callers { name, opts } => {
            let mode = opts.to_mode();
            let out = query::query_callers(&store, &root, &name, mode)?;
            let body = serde_json::to_string(&out)?;
            crabcc_core::track::record(
                "callers",
                &name,
                out.count(),
                &repo_label(&root),
                body.len(),
            );
            println!("{body}");
        }
        Cmd::Outline { file } => {
            let key = file.to_string_lossy();
            let syms = crabcc_core::outline::outline(&store, &key)?;
            let body = serde_json::to_string(&syms)?;
            crabcc_core::track::record("outline", &key, syms.len(), &repo_label(&root), body.len());
            println!("{body}");
        }
        Cmd::Files {
            under,
            lang,
            ext,
            limit,
        } => {
            let files = list_files(
                &store,
                under.as_deref(),
                lang.as_deref(),
                ext.as_deref(),
                limit,
            )?;
            let body = serde_json::to_string(&files)?;
            crabcc_core::track::record(
                "files",
                "list",
                files.len(),
                &repo_label(&root),
                body.len(),
            );
            println!("{body}");
        }
        Cmd::Grep { pattern } => {
            // TODO: ripgrep `grep` crate, annotate hits with enclosing symbol.
            println!("{{\"status\":\"todo\",\"op\":\"grep\",\"pattern\":\"{pattern}\"}}");
        }
        Cmd::Fuzzy { query, limit } => {
            let fts = crabcc_core::fts::Fts::open(&fts_dir)?;
            let hits = fts.fuzzy(&query, limit)?;
            let body = serde_json::to_string(&hits)?;
            crabcc_core::track::record("fuzzy", &query, hits.len(), &repo_label(&root), body.len());
            println!("{body}");
        }
        Cmd::Prefix { query, limit } => {
            let fts = crabcc_core::fts::Fts::open(&fts_dir)?;
            let hits = fts.prefix(&query, limit)?;
            let body = serde_json::to_string(&hits)?;
            crabcc_core::track::record(
                "prefix",
                &query,
                hits.len(),
                &repo_label(&root),
                body.len(),
            );
            println!("{body}");
        }
        Cmd::FtsRebuild => {
            let fts = crabcc_core::fts::Fts::open(&fts_dir)?;
            let n = fts.rebuild(&store)?;
            println!("{{\"indexed\":{n}}}");
        }
        Cmd::Track { json } => {
            let r = crabcc_core::track::report()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                print_track_human(&r);
            }
        }
        Cmd::Watch { debounce } => {
            let store = std::sync::Arc::new(std::sync::Mutex::new(store));
            crabcc_core::watch::watch(&root, store, std::time::Duration::from_millis(debounce))?;
        }
        Cmd::GraphBuild => {
            let g = crabcc_core::graph::CallGraph::build(&store, &root)?;
            let path = root.join(".crabcc").join("graph.json");
            g.save(&path)?;
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "edges":  g.edge_count,
                    "callers": g.callers.len(),
                    "callees": g.callees.len(),
                    "path":   path.to_string_lossy(),
                }))?
            );
        }
        Cmd::Graph {
            name,
            dir: direction,
            depth,
        } => {
            let path = root.join(".crabcc").join("graph.json");
            let g = if path.exists() {
                crabcc_core::graph::CallGraph::load(&path)?
            } else {
                // No cache: build on demand. Slower, but correct.
                eprintln!("crabcc graph: no .crabcc/graph.json — building on the fly (run `crabcc graph-build` to cache)");
                crabcc_core::graph::CallGraph::build(&store, &root)?
            };
            let hits = match direction.as_str() {
                "callees" => g.outgoing(&name, depth),
                _ => g.incoming(&name, depth),
            };
            let body = serde_json::to_string(&hits)?;
            crabcc_core::track::record("graph", &name, hits.len(), &repo_label(&root), body.len());
            println!("{body}");
        }
    }
    Ok(())
}

fn list_files(
    store: &Store,
    under: Option<&str>,
    lang: Option<&str>,
    ext: Option<&str>,
    limit: usize,
) -> Result<Vec<String>> {
    let all = store.list_files()?;
    let mut out: Vec<String> = all
        .into_iter()
        .filter(|(p, l)| {
            under.is_none_or(|u| p.starts_with(u))
                && lang.is_none_or(|want| l == want)
                && ext.is_none_or(|e| p.ends_with(&format!(".{e}")))
        })
        .map(|(p, _)| p)
        .collect();
    out.sort();
    if limit > 0 && out.len() > limit {
        out.truncate(limit);
    }
    Ok(out)
}

#[cfg(unix)]
fn reset_sigpipe() {
    // SAFETY: setting a signal handler is intrinsically unsafe, but
    // restoring the default behaviour for SIGPIPE is the conventional
    // fix for CLI tools that get piped into `head`.
    unsafe {
        let _ = libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

#[cfg(not(unix))]
fn reset_sigpipe() {}

fn repo_label(root: &Path) -> String {
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
    line("session", &r.session);
    line("last 24h", &r.last_24h);
    line("all-time", &r.all_time);
    if !r.by_op.is_empty() {
        println!("\nby operation:");
        for (op, b) in &r.by_op {
            line(op, b);
        }
    }
}
