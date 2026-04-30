use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use crabcc_core::{query, store::Store};
use std::path::{Path, PathBuf};

mod compress_cmd;
mod go;
mod install;
mod memory;

#[derive(Parser)]
#[command(name = "crabcc", version, about = "Symbol index for AI coding agents")]
struct Cli {
    /// Path to repo root (defaults to cwd).
    #[arg(long, global = true)]
    root: Option<PathBuf>,

    /// Run as MCP server over stdio instead of one-shot CLI.
    #[arg(long, global = true)]
    mcp: bool,

    /// Enable FSST compression at the storage layer (default: true).
    /// Pass `--compress=false` (or `--compress false`) to force plain text
    /// even if `.crabcc/fsst.symbols` exists. Read-side: encoded rows in the
    /// DB will be UNREADABLE (signatures surface as null) until `--compress`
    /// is re-enabled — they stay correct on disk; we just refuse to load the
    /// codec.
    #[arg(long, global = true, value_name = "BOOL", default_value_t = true, action = clap::ArgAction::Set)]
    compress: bool,

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
        /// Print human-readable text instead of JSON. JSON is the default for
        /// machine-readable output; pass `--text` for the columnar summary.
        #[arg(long)]
        text: bool,
    },
    /// Watch the repo and auto-`refresh` on file changes (Ctrl-C to exit).
    Watch {
        /// Debounce window in milliseconds — events within this window
        /// collapse into one refresh. Lower = more responsive, higher = less
        /// thrash on burst events like `git checkout`.
        #[arg(long, default_value_t = 500)]
        debounce: u64,
    },
    /// Call-graph operations: build, walk, cycles, orphans.
    Graph {
        #[command(subcommand)]
        op: GraphOp,
    },
    /// Symlink the crabcc skill + slash-command into `~/.claude/`, then
    /// print the `claude mcp add` invocation and hook JSON snippets to
    /// paste into `~/.claude/settings.json`. Never writes Claude config.
    InstallClaude {
        /// Skip the per-symlink y/N prompts.
        #[arg(long)]
        yes: bool,
        /// Print only the hook JSON to stdout (for piping to a file). Skips symlinks.
        #[arg(long)]
        print_hooks: bool,
    },
    /// Train an FSST symbol table from existing index data and write it to
    /// .crabcc/fsst.symbols, then re-encode rows on demand.
    Compress {
        /// Re-encode every existing symbol row in 1000-row batches.
        #[arg(long)]
        rebuild: bool,
        /// Print per-column byte savings (combine with --json for machine output).
        #[arg(long)]
        stats: bool,
        /// Emit stats as JSON instead of human-readable text. Implies --stats.
        #[arg(long)]
        json: bool,
        /// Override the index path (default: $CRABCC_DB or .crabcc/index.db).
        #[arg(long)]
        db: Option<PathBuf>,
        /// Probe in-process decode latency on N random encoded rows. Times
        /// `Codec::decompress` directly (no subprocess, no SQLite open) and
        /// emits p50/p95/p99 nanoseconds. Implies skipping train/rebuild.
        #[arg(long, value_name = "N")]
        decode_probe: Option<usize>,
    },
    /// AI memory operations (per-repo .crabcc/memory.db).
    Memory {
        #[command(subcommand)]
        sub: memory::MemoryCmd,
    },
    /// Check GitHub for a newer release; optionally clean local sidecars.
    /// Repo is private — uses `gh` for auth (run `gh auth login` first).
    Upgrade {
        /// Print the report and exit; never modify local state.
        #[arg(long)]
        check: bool,
        /// Print human-readable text instead of JSON. JSON is the default; pass
        /// `--text` for the formatted multi-line summary.
        #[arg(long)]
        text: bool,
        /// Apply local cleanup (rm `.crabcc/index.db`, tantivy/, graph.json)
        /// after the version check. Idempotent; safe to re-run. Re-index
        /// after with `crabcc index`.
        #[arg(long)]
        apply: bool,
        /// Override the GitHub repo to query (default: peterlodri-sec/crabcc,
        /// or `$CRABCC_UPGRADE_REPO` if set).
        #[arg(long)]
        repo: Option<String>,
    },
    /// Print a shell-completion script for the chosen shell to stdout.
    /// Pipe into the right location, e.g.:
    ///   crabcc completions zsh > ~/.local/share/zsh/site-functions/_crabcc
    Completions {
        /// Target shell.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Print build provenance (commit, branch, tag, time, target) plus a
    /// one-line project summary. Compile-time embedded — no runtime git lookup.
    Info {
        /// Print human-readable text instead of JSON. JSON is the default for
        /// machine consumers; pass `--text` for the indented banner.
        #[arg(long)]
        text: bool,
    },
    /// One-shot: index this repo (or refresh if already initialized),
    /// build the call-graph + memory store, then hand off to
    /// `claude --effort max --append-system-prompt <AGENTS.md>
    /// --no-chrome` so the LLM session starts pre-loaded with the
    /// crabcc primer + repo context.
    Go,
    /// Print the embedded OpenAPI 3.1 description of the MCP tool
    /// surface to stdout (canonical YAML). Pipe through `yq -o json`
    /// if you need JSON.
    Openapi,
}

#[derive(Subcommand)]
enum GraphOp {
    /// Rebuild the call-graph sidecar (.crabcc/graph.json) from the index.
    Build,
    /// BFS expansion: who calls / what does this symbol call?
    Walk {
        name: String,
        /// Direction: 'callers' (default) walks upward; 'callees' walks downward.
        #[arg(long, default_value = "callers")]
        dir: String,
        /// BFS depth limit.
        #[arg(long, default_value_t = 2)]
        depth: usize,
    },
    /// Find cycles: strongly-connected components of size ≥2 (mutual recursion).
    Cycles,
    /// List orphans: symbols that call others but have no incoming callers
    /// in the indexed graph. Useful as a dead-code triage starting point.
    Orphans,
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

    // `install-claude` is a config-only operation — it must run with no
    // store, no .crabcc dir, and no working repo (it resolves its own root
    // via `git rev-parse`). Handle it before we touch the SQLite store.
    if let Some(Cmd::InstallClaude { yes, print_hooks }) = &cli.cmd {
        return install::run(*yes, *print_hooks);
    }

    // `upgrade` and `completions` are also config-only — neither needs a
    // store. Run them before the SQLite open so they work in directories
    // that aren't repos.
    if let Some(Cmd::Upgrade {
        check,
        text,
        apply,
        repo,
    }) = cli.cmd.as_ref()
    {
        return run_upgrade(*check, *text, *apply, repo.as_deref(), &root);
    }
    if let Some(Cmd::Completions { shell }) = cli.cmd.as_ref() {
        let mut cmd = <Cli as clap::CommandFactory>::command();
        clap_complete::generate(*shell, &mut cmd, "crabcc", &mut std::io::stdout());
        return Ok(());
    }
    // `info` prints compile-time build provenance — no store, no .crabcc, no
    // working repo required. Run it before any filesystem touches.
    if let Some(Cmd::Info { text }) = cli.cmd.as_ref() {
        return run_info(*text);
    }

    // `go` is a top-level orchestrator that opens its own Store, indexes,
    // builds the graph, opens the memory palace, and execs claude. We
    // handle it before the global Store::open below because the indexing
    // step inside `go::run` is the canonical bootstrap path on a fresh
    // repo — pre-opening the store here would be wasted work.
    if let Some(Cmd::Go) = cli.cmd.as_ref() {
        return go::run(&root, &db);
    }

    // `openapi` is a config-only operation — embedded constant. Run it
    // before the store to keep it usable in non-repo cwds.
    if let Some(Cmd::Openapi) = cli.cmd.as_ref() {
        print!("{}", crabcc_mcp::OPENAPI_YAML);
        return Ok(());
    }

    // `compress` is a meta-operation on the index. It owns its own codec
    // lifecycle (we're MAKING the codec, not consuming one), so it bypasses
    // the global `Store::open` that would auto-load whatever is on disk.
    if let Some(Cmd::Compress {
        rebuild,
        stats,
        json,
        db: db_override,
        decode_probe,
    }) = cli.cmd.as_ref()
    {
        let db_path = db_override.clone().unwrap_or_else(|| db.clone());
        return compress_cmd::run(compress_cmd::Args {
            root: root.clone(),
            db: db_path,
            rebuild: *rebuild,
            // --json implies --stats (mirrors common CLI ergonomics).
            stats: *stats || *json,
            json: *json,
            decode_probe: *decode_probe,
        });
    }

    // Memory subcommands open `.crabcc/memory.db` directly via Palace —
    // route them here so we don't pay the symbol-index Store::open cost
    // on a memory-only invocation.
    if let Some(Cmd::Memory { sub }) = cli.cmd {
        return memory::run(&root, sub);
    }

    std::fs::create_dir_all(db.parent().unwrap())?;
    let store = Store::open_with_compress(&db, cli.compress)?;
    let fts_dir = root.join(".crabcc").join("tantivy");

    match cli.cmd.unwrap_or(Cmd::Index) {
        Cmd::Index => {
            let stats = crabcc_core::index::full_index(&root, &store)?;
            // Rebuild Tantivy too — keep the fuzzy/prefix sidecar in lockstep
            // with a full reindex. (refresh deliberately does not.)
            if let Ok(fts) = crabcc_core::fts::Fts::open(&fts_dir) {
                let _ = fts.rebuild(&store);
            }
            println!("{}", sonic_rs::to_string(&stats)?);
        }
        Cmd::Refresh => {
            let stats = crabcc_core::index::refresh(&root, &store)?;
            println!("{}", sonic_rs::to_string(&stats)?);
        }
        Cmd::Sym { name } => {
            let syms = query::find_symbol(&store, &name)?;
            let body = sonic_rs::to_string(&syms)?;
            crabcc_core::track::record("sym", &name, syms.len(), &repo_label(&root), body.len());
            memory::auto_capture(&root, "sym", &name, syms.len());
            println!("{body}");
        }
        Cmd::Refs { name, opts } => {
            let mode = opts.to_mode();
            let out = query::query_refs(&store, &root, &name, mode)?;
            let body = sonic_rs::to_string(&out)?;
            crabcc_core::track::record("refs", &name, out.count(), &repo_label(&root), body.len());
            memory::auto_capture(&root, "refs", &name, out.count());
            println!("{body}");
        }
        Cmd::Callers { name, opts } => {
            let mode = opts.to_mode();
            let out = query::query_callers(&store, &root, &name, mode)?;
            let body = sonic_rs::to_string(&out)?;
            crabcc_core::track::record(
                "callers",
                &name,
                out.count(),
                &repo_label(&root),
                body.len(),
            );
            memory::auto_capture(&root, "callers", &name, out.count());
            println!("{body}");
        }
        Cmd::Outline { file } => {
            let key = file.to_string_lossy();
            let syms = crabcc_core::outline::outline(&store, &key)?;
            let body = sonic_rs::to_string(&syms)?;
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
            let body = sonic_rs::to_string(&files)?;
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
            let body = sonic_rs::to_string(&hits)?;
            crabcc_core::track::record("fuzzy", &query, hits.len(), &repo_label(&root), body.len());
            memory::auto_capture(&root, "fuzzy", &query, hits.len());
            println!("{body}");
        }
        Cmd::Prefix { query, limit } => {
            let fts = crabcc_core::fts::Fts::open(&fts_dir)?;
            let hits = fts.prefix(&query, limit)?;
            let body = sonic_rs::to_string(&hits)?;
            crabcc_core::track::record(
                "prefix",
                &query,
                hits.len(),
                &repo_label(&root),
                body.len(),
            );
            memory::auto_capture(&root, "prefix", &query, hits.len());
            println!("{body}");
        }
        Cmd::FtsRebuild => {
            let fts = crabcc_core::fts::Fts::open(&fts_dir)?;
            let n = fts.rebuild(&store)?;
            println!("{{\"indexed\":{n}}}");
        }
        Cmd::Track { text } => {
            let r = crabcc_core::track::report()?;
            if text {
                print_track_human(&r);
            } else {
                println!("{}", sonic_rs::to_string_pretty(&r)?);
            }
        }
        Cmd::Watch { debounce } => {
            let store = std::sync::Arc::new(std::sync::Mutex::new(store));
            crabcc_core::watch::watch(&root, store, std::time::Duration::from_millis(debounce))?;
        }
        Cmd::Graph { op } => match op {
            GraphOp::Build => {
                let g = crabcc_core::graph::CallGraph::build(&store, &root)?;
                let path = root.join(".crabcc").join("graph.json");
                g.save(&path)?;
                println!(
                    "{}",
                    sonic_rs::to_string(&serde_json::json!({
                        "edges":   g.edge_count,
                        "callers": g.callers.len(),
                        "callees": g.callees.len(),
                        "path":    path.to_string_lossy(),
                    }))?
                );
            }
            GraphOp::Walk {
                name,
                dir: direction,
                depth,
            } => {
                let path = root.join(".crabcc").join("graph.json");
                let g = if path.exists() {
                    crabcc_core::graph::CallGraph::load(&path)?
                } else {
                    eprintln!(
                        "crabcc graph walk: no .crabcc/graph.json — building on the fly (run `crabcc graph build` to cache)"
                    );
                    crabcc_core::graph::CallGraph::build(&store, &root)?
                };
                let hits = match direction.as_str() {
                    "callees" => g.outgoing(&name, depth),
                    _ => g.incoming(&name, depth),
                };
                let body = sonic_rs::to_string(&hits)?;
                crabcc_core::track::record(
                    "graph",
                    &name,
                    hits.len(),
                    &repo_label(&root),
                    body.len(),
                );
                println!("{body}");
            }
            GraphOp::Cycles => {
                let g = load_or_build_graph(&store, &root)?;
                let cycles = g.cycles();
                let body = sonic_rs::to_string(&cycles)?;
                crabcc_core::track::record(
                    "graph-cycles",
                    "cycles",
                    cycles.len(),
                    &repo_label(&root),
                    body.len(),
                );
                println!("{body}");
            }
            GraphOp::Orphans => {
                let g = load_or_build_graph(&store, &root)?;
                let orphans = g.orphans();
                let body = sonic_rs::to_string(&orphans)?;
                crabcc_core::track::record(
                    "graph-orphans",
                    "orphans",
                    orphans.len(),
                    &repo_label(&root),
                    body.len(),
                );
                println!("{body}");
            }
        },
        // Handled by the early-return branches above before the store opens.
        Cmd::InstallClaude { .. } => unreachable!("install-claude handled before store init"),
        Cmd::Compress { .. } => unreachable!("compress handled before store init"),
        Cmd::Memory { .. } => unreachable!("memory handled before store init"),
        Cmd::Go => unreachable!("go handled before store init"),
        Cmd::Openapi => unreachable!("openapi handled before store init"),
        Cmd::Upgrade { .. } => unreachable!("upgrade handled before store init"),
        Cmd::Completions { .. } => unreachable!("completions handled before store init"),
        Cmd::Info { .. } => unreachable!("info handled before store init"),
    }
    Ok(())
}

fn load_or_build_graph(store: &Store, root: &Path) -> Result<crabcc_core::graph::CallGraph> {
    let path = root.join(".crabcc").join("graph.json");
    if path.exists() {
        crabcc_core::graph::CallGraph::load(&path)
    } else {
        crabcc_core::graph::CallGraph::build(store, root)
    }
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

fn run_upgrade(
    check: bool,
    text: bool,
    apply: bool,
    repo_override: Option<&str>,
    root: &Path,
) -> Result<()> {
    use crabcc_core::upgrade;
    let repo = repo_override
        .map(String::from)
        .unwrap_or_else(upgrade::target_repo);
    let report = upgrade::build_report(&repo, Some(root));

    if text {
        print_upgrade_human(&report, &repo);
    } else {
        println!("{}", sonic_rs::to_string_pretty(&report)?);
    }

    if check {
        return Ok(());
    }

    // Only the `--apply` path mutates anything. Without `--apply` the command
    // is read-only — we print the recommendations and exit.
    if apply {
        match upgrade::cleanup_index(root) {
            Ok(_) => {
                if text {
                    eprintln!(
                        "\n  cleaned `.crabcc/{{index.db,tantivy,graph.json}}` — run \
                         `crabcc index` to rebuild."
                    );
                }
            }
            Err(e) => {
                eprintln!("warning: cleanup failed: {e}");
            }
        }
    }
    Ok(())
}

fn print_upgrade_human(r: &crabcc_core::upgrade::UpgradeReport, repo: &str) {
    use crabcc_core::upgrade::{BumpKind, VersionDelta};
    println!("crabcc upgrade — {repo}");
    println!("  installed: {}", r.installed);
    if let Some(rel) = &r.latest {
        let when = rel.published_at.as_deref().unwrap_or("");
        println!("  latest:    {} ({})", rel.tag, when.trim());
    } else {
        println!("  latest:    <unknown>");
    }
    let status = match &r.delta {
        VersionDelta::UpToDate => "up to date".to_string(),
        VersionDelta::Newer { kind, .. } => format!(
            "{} bump available",
            match kind {
                BumpKind::Patch => "patch",
                BumpKind::Minor => "minor",
                BumpKind::Major => "MAJOR",
            }
        ),
        VersionDelta::Ahead { .. } => "ahead of latest release (dev build)".into(),
        VersionDelta::Unknown { reason } => format!("unknown ({reason})"),
    };
    println!("  status:    {status}");
    if !r.recommendations.is_empty() {
        println!("\nrecommendations:");
        for rec in &r.recommendations {
            println!("  • {rec}");
        }
    }
}

/// Compile-time build provenance — populated by `build.rs` via cargo:rustc-env.
/// Always-present: version, target. Possibly empty: tag (only when HEAD is tagged).
struct BuildInfo {
    version: &'static str,
    commit: &'static str,
    branch: &'static str,
    tag: &'static str,
    time: &'static str,
    target: &'static str,
}

const BUILD: BuildInfo = BuildInfo {
    version: env!("CARGO_PKG_VERSION"),
    commit: env!("CRABCC_BUILD_COMMIT"),
    branch: env!("CRABCC_BUILD_BRANCH"),
    tag: env!("CRABCC_BUILD_TAG"),
    time: env!("CRABCC_BUILD_TIME"),
    target: env!("CRABCC_BUILD_TARGET"),
};

// Compressed one-line summary: kept short enough for an LLM context tag,
// terminal status line, and the `--version`-style banner. Hand-curated
// rather than auto-counted so the wording stays right when langs grow.
const PROJECT_SUMMARY: &str =
    "Symbol index for AI coding agents. 7 langs (TS/TSX/JS/Ruby/Rust/Go/Python). \
     11 MCP tools. SQLite + Tantivy + ast-grep. Token-shaping flags collapse \
     16k-token results to ~3 tokens. 47–5500x faster than grep -rn on monorepos.";

fn run_info(text: bool) -> Result<()> {
    if !text {
        // JSON is the default — `serde_json::json!` builds the value via
        // serde, then sonic-rs serializes it. We keep `serde_json::json!`
        // because sonic-rs doesn't ship its own json! macro in 0.3, and
        // the encode side is what matters for performance anyway.
        let v = serde_json::json!({
            "version":  BUILD.version,
            "commit":   BUILD.commit,
            "branch":   BUILD.branch,
            "tag":      if BUILD.tag.is_empty() { serde_json::Value::Null }
                        else { serde_json::Value::String(BUILD.tag.into()) },
            "time":     BUILD.time,
            "target":   BUILD.target,
            "summary":  PROJECT_SUMMARY,
        });
        println!("{}", sonic_rs::to_string_pretty(&v)?);
        return Ok(());
    }

    // The compressed one-liner the user explicitly asked for: useful for
    // status lines, bug reports, or paste-into-issue contexts.
    println!(
        "crabcc v{} ({}, {}, {}, {})",
        BUILD.version,
        BUILD.commit,
        if BUILD.tag.is_empty() {
            BUILD.branch
        } else {
            BUILD.tag
        },
        BUILD.time,
        BUILD.target,
    );
    println!();
    println!("  version:  {}", BUILD.version);
    println!("  commit:   {}", BUILD.commit);
    println!("  branch:   {}", BUILD.branch);
    println!(
        "  tag:      {}",
        if BUILD.tag.is_empty() {
            "—"
        } else {
            BUILD.tag
        }
    );
    println!("  built:    {}", BUILD.time);
    println!("  target:   {}", BUILD.target);
    println!();
    println!("  {}", PROJECT_SUMMARY);
    Ok(())
}
