use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use crabcc_core::{query, store::Store};
use std::path::{Path, PathBuf};

mod agent;
mod agent_guard;
mod agent_runs_db;
mod compress_cmd;
mod doctor;
mod go;
mod install;
mod memory;
mod status;
mod telemetry;

#[derive(Parser)]
#[command(name = "crabcc", version, about = "Symbol index for AI coding agents")]
struct Cli {
    /// Path to repo root (defaults to cwd).
    #[arg(long, global = true)]
    root: Option<PathBuf>,

    /// Run as MCP server over stdio instead of one-shot CLI.
    #[arg(long, global = true)]
    mcp: bool,

    /// MCP server only — expose the dev / diagnostic surface
    /// (`_openapi`, `_health`). Default is the slimmer agent-facing
    /// surface (issue #59). Equivalent to setting `CRABCC_MCP_DEV=1`.
    #[arg(long, global = true)]
    dev: bool,

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
    /// Incremental refresh — re-reads disk vs the stored mtime + sha for each
    /// indexed file. Default output is `RefreshStats` (counts only); pass
    /// `--delta` to also receive the per-bucket file lists (`added` /
    /// `modified` / `removed`) so an agent can re-read just what changed.
    Refresh {
        /// Emit `{"added": [...], "modified": [...], "removed": [...], "stats": {...}}`
        /// instead of bare counts. The lists exclude `touched` files
        /// (mtime bumped, content unchanged) — agents care about *content*
        /// deltas, not metadata.
        #[arg(long)]
        delta: bool,
    },
    /// Find a symbol by name.
    Sym {
        name: String,
        /// Restrict results to files changed since this git revision.
        /// See `refs --since` for the accepted shape.
        #[arg(long, value_name = "GIT_REV")]
        since: Option<String>,
    },
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
    /// Ollama auth-stack operations: up, down, status, logs, pull (issue #105).
    /// Operator surface — manipulates Docker Compose against the bundled
    /// stack at `install/ollama-stack/` (or `$CRABCC_OLLAMA_STACK_DIR`).
    /// Output is JSON by default for machine consumers (the menubar app +
    /// Chrome extension from issue #107 read this surface).
    #[command(name = "ollama-stack")]
    OllamaStack {
        #[command(subcommand)]
        op: OllamaStackOp,
    },
    /// Diagnostic surface — issue #107 Phase 5a. JSON by default for the
    /// menubar app + Chrome extension; pass `--text` for the human-readable
    /// checklist. Subcommands: `docker` (preflight + OrbStack detection),
    /// `stack` (Compose health + container list), `keys` (~/.crabcc.local
    /// .api-key + .env mode + parity). No subcommand = run all + aggregate.
    Doctor {
        #[command(subcommand)]
        op: Option<DoctorOp>,
        /// Format the report as a human checklist instead of JSON.
        #[arg(long)]
        text: bool,
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
        /// Issue #105 — materialize the Ollama auth stack at
        /// `$HOME/.crabcc/ollama-stack/` (Compose recipe + Caddyfile +
        /// LiteLLM config + init-keys.sh + README), then run
        /// `docker compose up -d --wait`. Requires Docker.
        #[arg(long)]
        with_ollama_stack: bool,
        /// Print the Ollama-stack bring-up commands and exit. Skips
        /// symlinks AND the materialization step. Counterpart of
        /// `--print-hooks` for the auth-stack recipe.
        #[arg(long)]
        print_stack_instructions: bool,
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
        /// Issue #105 — also refresh the bundled Ollama auth stack:
        /// `docker compose pull` against `$HOME/.crabcc/ollama-stack/`
        /// (or wherever `$CRABCC_OLLAMA_STACK_DIR` points). Combine with
        /// `--apply` to also re-up the stack so changed images are
        /// picked up.
        #[arg(long)]
        with_stack: bool,
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
    ///
    /// Status-line variants (issue #43): `--status-line` emits a terse,
    /// p95<50ms one-liner suitable for Starship / tmux / VS Code status
    /// bars. `--is-repo` exits 0 inside a crabcc-indexed repo, 1
    /// otherwise — used by Starship's `when = …` gate.
    Info {
        /// Print human-readable text instead of JSON. JSON is the default for
        /// machine consumers; pass `--text` for the indented banner.
        #[arg(long)]
        text: bool,
        /// Emit a render-budget-friendly status-line summary (token savings,
        /// index age, memory drawers, Claude Code activity). Pair with
        /// `--json` for a machine-readable shape.
        #[arg(long)]
        status_line: bool,
        /// Exit 0 if cwd is inside a crabcc-indexed repo (`.crabcc/index.db`
        /// reachable), 1 otherwise. No stdout. Used by Starship `when`.
        #[arg(long)]
        is_repo: bool,
        /// JSON output for status-line. No-op without `--status-line`.
        #[arg(long)]
        json: bool,
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
    /// Drive an LLM agent through one round of tool-use against the
    /// crabcc MCP surface (issue #62). Today the runtime is a host
    /// subprocess (same trust as `crabcc go`); a microsandbox-backed
    /// runtime drops in under the `agent-sandbox` cargo feature for v3.0
    /// — see `install/agent-runtime.md` for the design + threat model.
    Agent {
        /// The user prompt — the agent's first turn. Quote freely.
        #[arg(long, value_name = "PROMPT")]
        run: String,
        /// Print the planned invocation (binary, system prompt, model)
        /// instead of spawning the agent. Lets you verify wiring + the
        /// resolved AGENTS.md without burning tokens.
        #[arg(long)]
        dry_run: bool,
        /// Override the model the agent uses. None = the agent CLI's
        /// default (Claude Code = whatever `claude` was last configured
        /// with, typically the latest Sonnet/Opus).
        #[arg(long, value_name = "ID")]
        model: Option<String>,
        /// Skip the implicit `crabcc refresh` before launch. Useful when
        /// a wrapping script has already brought the index up to date.
        #[arg(long)]
        no_refresh: bool,
        /// LLM backend the spawned agent talks to. `claude` (default)
        /// uses Anthropic via Claude Code's existing config. `ollama`
        /// routes through the local LiteLLM proxy from the bundled
        /// Compose stack (issue #105) — requires Docker; the stack is
        /// auto-brought-up before spawn via `ollama_stack::ensure_up`.
        #[arg(long, value_name = "BACKEND", default_value = "claude")]
        backend: String,
    },
    /// Start the localhost call-graph viewer (issue #64). Binds to
    /// 127.0.0.1 by default — pass `--bind 0.0.0.0` only on a trusted
    /// LAN; the server is unauthenticated and exposes architecture.
    Serve {
        /// TCP port to bind. 0 picks an ephemeral port (used by tests).
        #[arg(long, default_value_t = 7878)]
        port: u16,
        /// Bind address. Default `127.0.0.1`. Override only when you
        /// need access from another machine on a trusted network.
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
        /// Skip auto-opening the system browser after the server starts.
        #[arg(long)]
        no_open: bool,
        /// Skip the `go::init`-equivalent index/graph/memory bootstrap
        /// at startup. Default behaviour brings the repo to a fully-
        /// initialized state so the live dashboard's first /api/bootstrap
        /// poll has real numbers; pass this when wrapping `crabcc serve`
        /// in a supervisor that's already done it.
        #[arg(long)]
        no_init: bool,
    },
    /// List agent runs from the singleton ~/.crabcc/_internal.db. Used
    /// by the macOS menubar Status section + `task agent-status`.
    #[command(name = "agent-ls")]
    AgentLs {
        /// Only show rows with status='running' (live agents).
        #[arg(long)]
        active_only: bool,
        /// Cap the result set. Defaults to 50.
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// JSON output (one object per line — easier to grep than --json).
        #[arg(long)]
        json: bool,
    },
    /// Sweep stuck / zombie agent runs. Wired to a 20-min LaunchAgent
    /// (com.crabcc.agent-guard.plist). Marks runs whose PID is gone as
    /// 'crashed' (zombie reap) and SIGTERM/SIGKILL runs whose log file
    /// hasn't been written to in --idle-secs (default 1800 = 30 min).
    /// Records every action in agent_kill_events + writes a per-run
    /// kill log at ~/.crabcc/agents/<id>/.agent-<id>-kill-log.
    #[command(name = "agent-guard")]
    AgentGuard {
        /// Idle threshold in seconds. Default 1800 (30 min).
        #[arg(long, default_value_t = 1800u64)]
        idle_secs: u64,
        /// Grace period in ms between SIGTERM and SIGKILL.
        #[arg(long, default_value_t = 5000u64)]
        term_grace_ms: u64,
        /// JSON summary (single line — easy for the LaunchAgent to log).
        #[arg(long)]
        json: bool,
    },
    /// List agent kill events from the audit trail. The web UI / viz
    /// dashboard filters on this surface to show "incidents only".
    #[command(name = "agent-kills")]
    AgentKills {
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
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

#[derive(Subcommand)]
enum DoctorOp {
    /// `docker --version` + `docker compose version` + OrbStack detection.
    Docker,
    /// Bundled Compose-stack health: per-container JSON via
    /// `ollama_stack::status`, plus an `unhealthy` summary.
    Stack,
    /// Inspect `~/.crabcc.local.api-key` and the auth-stack `.env`:
    /// presence, file mode, master-key parity.
    Keys,
    /// Agent runtime readiness: `claude` binary on PATH and/or
    /// `OLLAMA_BASE_URL`+`OLLAMA_API_KEY` env vars set. Doesn't invoke
    /// the agent — use `crabcc agent --dry-run` for that.
    Agent,
    /// Jobs queue reachability: shells out to `redis-cli ping` against
    /// `$REDIS_URL` (default `redis://127.0.0.1:6379`). Issue #109.
    Jobs,
}

#[derive(Subcommand)]
enum OllamaStackOp {
    /// `docker compose up -d --wait` against the resolved stack. Blocks
    /// until every healthcheck reports green or compose times out.
    Up,
    /// `docker compose down`. Pass `--volumes` to wipe the named
    /// volumes (model cache, caddy data) — default keeps them so the
    /// next `up` is warm.
    Down {
        #[arg(long)]
        volumes: bool,
    },
    /// `docker compose ps --format json` + per-container `docker inspect`,
    /// returned as a JSON array of `ContainerInfo` rows.
    Status,
    /// Tail of `docker compose logs`. Pass a service name to scope.
    Logs {
        /// Service to tail. Omit for all services.
        service: Option<String>,
        /// Number of lines from the tail.
        #[arg(long, default_value_t = 100)]
        tail: usize,
    },
    /// `docker compose pull` to refresh upstream images. Combine with
    /// `up` afterward to recreate services whose image digest changed.
    Pull,
}

/// Shaping flags for refs/callers. `--files-only`, `--summary`, and
/// `--count` are mutually exclusive output shapes; `--limit` modifies
/// whichever shape you picked (or the default hit-list shape).
#[derive(Args, Debug, Clone)]
struct ResultOpts {
    /// Cap result size at N. 0 = unlimited. Applies to hits, files, or
    /// (for `--summary`) the per-file count map (keys sorted by path).
    #[arg(long, default_value_t = 0)]
    limit: usize,
    /// Emit deduped JSON array of file paths only — no line/col/snippet.
    #[arg(long, conflicts_with_all = ["count", "summary"])]
    files_only: bool,
    /// Emit `{"by_file": {"path": N, ...}}` — per-file hit-count
    /// distribution. Useful when an agent needs distribution-shape, not
    /// individual matches. ~95% bytes saved vs raw hits.
    #[arg(long, conflicts_with_all = ["count", "files_only"])]
    summary: bool,
    /// Emit `{"count": N}` only — no per-hit payload.
    #[arg(long)]
    count: bool,
    /// Cache-revalidation hint. Pass the fingerprint from a previous
    /// call; if the result is unchanged, the response is just
    /// `{"unchanged":true,"fingerprint":"..."}` (zero hits payload).
    /// Otherwise the response is wrapped as
    /// `{"fingerprint":"<new>","result":<existing-shape>}`.
    /// When this flag is omitted, the result body is returned verbatim
    /// — backwards-compatible for callers that don't opt in.
    #[arg(long, value_name = "FINGERPRINT")]
    if_changed: Option<String>,
    /// Restrict results to files that changed since this git revision.
    /// Accepts anything `git diff` accepts: a SHA prefix (`abc1234`), a
    /// ref (`origin/main`), or a relative ref (`HEAD~5`). Internally
    /// resolves to `git diff --name-only --diff-filter=AMR <SINCE>...HEAD`
    /// and filters every per-file lookup to that set.
    #[arg(long, value_name = "GIT_REV")]
    since: Option<String>,
    /// Emit one JSON hit per line (NDJSON) instead of a single JSON
    /// array. Lets a streaming consumer peek-and-stop without loading
    /// the whole response into memory. Hits-mode only — combining
    /// with `--count` / `--files-only` / `--summary` is rejected.
    #[arg(
        long,
        conflicts_with_all = ["count", "files_only", "summary", "if_changed"],
    )]
    ndjson: bool,
}

impl ResultOpts {
    fn to_mode(&self) -> query::Mode {
        if self.count {
            query::Mode::Count
        } else if self.summary {
            query::Mode::Summary {
                limit: opt(self.limit),
            }
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

/// NDJSON emit for refs/callers Hits output. One JSON object per line on
/// stdout — lets a streaming consumer peek-and-stop without loading the
/// whole response into memory. Non-Hits modes go through the regular
/// JSON path; the CLI flag rejects the combo at parse time so this
/// helper only sees `Output::Hits`.
fn emit_hits_ndjson(out: &query::Output) -> Result<()> {
    let query::Output::Hits(hits) = out else {
        // Belt-and-braces — clap conflicts already reject the mix.
        anyhow::bail!("--ndjson requires hits-mode output");
    };
    let stdout = std::io::stdout();
    let mut w = stdout.lock();
    use std::io::Write;
    for h in hits {
        let line = sonic_rs::to_string(h)?;
        writeln!(w, "{line}")?;
    }
    Ok(())
}

fn main() -> Result<()> {
    // Issue #90 — KPI-focused tracing init. Default release filter is
    // `crabcc*=info,warn` so MCP tool calls + graph stats surface but
    // third-party chatter (tantivy commits, sqlite stmts) stays at warn.
    // Hold the guard for the lifetime of main so the non-blocking
    // writer flushes on shutdown.
    let _telemetry = telemetry::init();
    // Closed-pipe (e.g. piping to `head`) should exit silently, not panic.
    reset_sigpipe();

    let cli = Cli::parse();
    let root = cli.root.unwrap_or_else(|| std::env::current_dir().unwrap());
    let db = root.join(".crabcc").join("index.db");

    // Issue #74 — `crabcc` is the low-level surface; `ccc` is the
    // user-friendly combo CLI. Emit a one-line stderr hint when the
    // user invokes a granular query verb directly. Suppressed when
    // called from `ccc` (CCC_NO_WARN=1), in scripts/pipelines (stderr
    // not a tty), or by user opt-out (CRABCC_NO_HINT=1).
    if std::env::var_os("CCC_NO_WARN").is_none()
        && std::env::var_os("CRABCC_NO_HINT").is_none()
        && atty_stderr()
    {
        let hint: Option<&str> = match &cli.cmd {
            Some(Cmd::Sym { .. }) => Some("ccc find <NAME>"),
            Some(Cmd::Refs { .. }) => Some("ccc find <NAME> --mode references"),
            Some(Cmd::Callers { .. }) => Some("ccc find <NAME> --mode callers"),
            Some(Cmd::Fuzzy { .. }) => Some("ccc find <NAME> --mode fuzzy"),
            Some(Cmd::Prefix { .. }) => Some("ccc find <NAME> --mode prefix"),
            Some(Cmd::Grep { .. }) => Some("ccc find <PATTERN> --mode grep"),
            Some(Cmd::Files { .. }) => Some("ccc list --files"),
            _ => None,
        };
        if let Some(h) = hint {
            eprintln!(
                "note: `crabcc` is the low-level surface; equivalent: `{h}` \
                 (suppress with CRABCC_NO_HINT=1)"
            );
        }
    }

    if cli.mcp {
        // `--dev` flag OR `CRABCC_MCP_DEV=1` env both flip the dev surface
        // on. The CLI flag wins because it's more explicit; if neither is
        // set, the slim default surface is used (issue #59).
        let dev = cli.dev || crabcc_mcp::dev_mode_from_env();
        return crabcc_mcp::serve_stdio_with(&root, dev);
    }

    // `install-claude` is a config-only operation — it must run with no
    // store, no .crabcc dir, and no working repo (it resolves its own root
    // via `git rev-parse`). Handle it before we touch the SQLite store.
    if let Some(Cmd::InstallClaude {
        yes,
        print_hooks,
        with_ollama_stack,
        print_stack_instructions,
    }) = &cli.cmd
    {
        return install::run(install::InstallOptions {
            yes: *yes,
            print_hooks_only: *print_hooks,
            with_ollama_stack: *with_ollama_stack,
            print_stack_instructions: *print_stack_instructions,
        });
    }

    // `upgrade` and `completions` are also config-only — neither needs a
    // store. Run them before the SQLite open so they work in directories
    // that aren't repos.
    if let Some(Cmd::Upgrade {
        check,
        text,
        apply,
        repo,
        with_stack,
    }) = cli.cmd.as_ref()
    {
        return run_upgrade(*check, *text, *apply, *with_stack, repo.as_deref(), &root);
    }
    if let Some(Cmd::Completions { shell }) = cli.cmd.as_ref() {
        let mut cmd = <Cli as clap::CommandFactory>::command();
        clap_complete::generate(*shell, &mut cmd, "crabcc", &mut std::io::stdout());
        return Ok(());
    }
    // `info` prints compile-time build provenance — no store, no .crabcc, no
    // working repo required. Run it before any filesystem touches.
    if let Some(Cmd::Info {
        text,
        status_line,
        is_repo,
        json,
    }) = cli.cmd.as_ref()
    {
        // --is-repo is the Starship gate: exit code only, no stdout. We
        // bypass the rest of the binary so the round-trip stays cheap.
        if *is_repo {
            std::process::exit(if status::is_repo(&root) { 0 } else { 1 });
        }
        if *status_line {
            return status::run_status_line(&root, *json);
        }
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

    // `serve` (issue #64) opens the Store lazily per HTTP request — the
    // foreground process is the listen loop, not a one-shot CLI op.
    // Bypass the global Store::open so the server starts even before the
    // first index has been built (the viewer surfaces a clear 400 then).
    if let Some(Cmd::Serve {
        port,
        bind,
        no_open,
        no_init,
    }) = cli.cmd.as_ref()
    {
        return run_serve(&root, *port, bind, *no_open, !*no_init);
    }

    // `agent --run` (issue #62) execs an agent CLI in the user's shell;
    // we deliberately don't open the symbol Store here because the agent
    // owns its lifecycle through MCP tool calls. `agent::run` handles a
    // best-effort `crabcc refresh` itself when `--no-refresh` is absent.
    if let Some(Cmd::Agent {
        run,
        dry_run,
        model,
        no_refresh,
        backend,
    }) = cli.cmd.as_ref()
    {
        let backend = agent::Backend::from_str(backend)?;
        return agent::run(agent::AgentRequest {
            prompt: run,
            root: &root,
            dry_run: *dry_run,
            model: model.clone(),
            no_refresh: *no_refresh,
            backend,
        });
    }

    // agent-ls / agent-guard / agent-kills — no Store needed, all read
    // from / write to ~/.crabcc/_internal.db directly.
    if let Some(Cmd::AgentLs {
        active_only,
        limit,
        json,
    }) = cli.cmd.as_ref()
    {
        return run_agent_ls(*active_only, *limit, *json);
    }
    if let Some(Cmd::AgentGuard {
        idle_secs,
        term_grace_ms,
        json,
    }) = cli.cmd.as_ref()
    {
        return agent_guard::run(agent_guard::GuardConfig {
            idle_secs: *idle_secs,
            term_grace_ms: *term_grace_ms,
            json: *json,
        });
    }
    if let Some(Cmd::AgentKills { limit, json }) = cli.cmd.as_ref() {
        return run_agent_kills(*limit, *json);
    }

    // `ollama-stack` is an operator surface — pure shell-out to
    // `docker compose` against the bundled stack at install/ollama-stack/.
    // No symbol Store needed (issue #105 Phase 3).
    if let Some(Cmd::OllamaStack { op }) = cli.cmd.as_ref() {
        return run_ollama_stack(op);
    }

    // `doctor` is the diagnostic surface (issue #107 Phase 5a). No Store
    // touched; each subcommand is read-only against the local environment.
    if let Some(Cmd::Doctor { op, text }) = cli.cmd.as_ref() {
        return match op {
            None => doctor::run_all(*text),
            Some(DoctorOp::Docker) => doctor::run_docker(*text),
            Some(DoctorOp::Stack) => doctor::run_stack(*text),
            Some(DoctorOp::Keys) => doctor::run_keys(*text),
            Some(DoctorOp::Agent) => doctor::run_agent(*text),
            Some(DoctorOp::Jobs) => doctor::run_jobs(*text),
        };
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

    let dispatched_cmd = cli.cmd.unwrap_or(Cmd::Index);
    tracing::info!(
        target: "crabcc_cli",
        cmd = cmd_name_for_log(&dispatched_cmd),
        "command: enter"
    );

    match dispatched_cmd {
        Cmd::Index => {
            let stats = crabcc_core::index::full_index(&root, &store)?;
            // Rebuild Tantivy too — keep the fuzzy/prefix sidecar in lockstep
            // with a full reindex. (refresh deliberately does not.)
            if let Ok(fts) = crabcc_core::fts::Fts::open(&fts_dir) {
                let _ = fts.rebuild(&store);
            }
            println!("{}", sonic_rs::to_string(&stats)?);
        }
        Cmd::Refresh { delta } => {
            if delta {
                let d = crabcc_core::index::refresh_delta(&root, &store)?;
                println!("{}", sonic_rs::to_string(&d)?);
            } else {
                let stats = crabcc_core::index::refresh(&root, &store)?;
                println!("{}", sonic_rs::to_string(&stats)?);
            }
        }
        Cmd::Sym { name, since } => {
            let syms = match since.as_deref() {
                Some(rev) => {
                    let files = crabcc_core::gitdiff::changed_files_since(&root, rev)?;
                    query::find_symbol_in_files(&store, &name, &files)?
                }
                None => query::find_symbol(&store, &name)?,
            };
            let body = sonic_rs::to_string(&syms)?;
            crabcc_core::track::record("sym", &name, syms.len(), &repo_label(&root), body.len());
            memory::auto_capture(&root, "sym", &name, syms.len());
            println!("{body}");
        }
        Cmd::Refs { name, opts } => {
            let mode = opts.to_mode();
            let files = match opts.since.as_deref() {
                Some(rev) => Some(crabcc_core::gitdiff::changed_files_since(&root, rev)?),
                None => None,
            };
            let out = query::query_refs(&store, &root, &name, mode, files.as_ref())?;
            let body = sonic_rs::to_string(&out)?;
            crabcc_core::track::record("refs", &name, out.count(), &repo_label(&root), body.len());
            memory::auto_capture(&root, "refs", &name, out.count());
            if opts.ndjson {
                emit_hits_ndjson(&out)?;
            } else {
                let envelope =
                    crabcc_core::hash::fingerprint_envelope(&body, opts.if_changed.as_deref());
                println!("{envelope}");
            }
        }
        Cmd::Callers { name, opts } => {
            let mode = opts.to_mode();
            let files = match opts.since.as_deref() {
                Some(rev) => Some(crabcc_core::gitdiff::changed_files_since(&root, rev)?),
                None => None,
            };
            let out = query::query_callers(&store, &root, &name, mode, files.as_ref())?;
            let body = sonic_rs::to_string(&out)?;
            crabcc_core::track::record(
                "callers",
                &name,
                out.count(),
                &repo_label(&root),
                body.len(),
            );
            memory::auto_capture(&root, "callers", &name, out.count());
            if opts.ndjson {
                emit_hits_ndjson(&out)?;
            } else {
                let envelope =
                    crabcc_core::hash::fingerprint_envelope(&body, opts.if_changed.as_deref());
                println!("{envelope}");
            }
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
        Cmd::Serve { .. } => unreachable!("serve handled before store init"),
        Cmd::Agent { .. } => unreachable!("agent handled before store init"),
        Cmd::AgentLs { .. } => unreachable!("agent-ls handled before store init"),
        Cmd::AgentGuard { .. } => unreachable!("agent-guard handled before store init"),
        Cmd::AgentKills { .. } => unreachable!("agent-kills handled before store init"),
        Cmd::OllamaStack { .. } => unreachable!("ollama-stack handled before store init"),
        Cmd::Doctor { .. } => unreachable!("doctor handled before store init"),
    }
    Ok(())
}

fn run_agent_ls(active_only: bool, limit: usize, json: bool) -> Result<()> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME not set"))?;
    let db_path = agent_runs_db::default_db_path(&home);
    if !db_path.exists() {
        if json {
            println!("[]");
        } else {
            eprintln!(
                "crabcc agent-ls: no DB at {} (no agent runs yet)",
                db_path.display()
            );
        }
        return Ok(());
    }
    let conn = agent_runs_db::open(&db_path)?;
    let _ = agent_runs_db::reap_stale(&conn);
    let rows = agent_runs_db::list_runs(&conn, active_only, limit)?;
    if json {
        let parts: Vec<String> = rows
            .iter()
            .map(|r| {
                format!(
                    r#"{{"id":"{}","status":"{}","started_ts":{},"finished_ts":{},"pid":{},"repo":"{}","runtime":"{}","model":"{}","exit_code":{}}}"#,
                    r.id,
                    r.status,
                    r.started_ts,
                    r.finished_ts.map(|v| v.to_string()).unwrap_or_else(|| "null".into()),
                    r.pid.map(|v| v.to_string()).unwrap_or_else(|| "null".into()),
                    r.repo.replace('"', "\\\""),
                    r.runtime.clone().unwrap_or_default(),
                    r.model.clone().unwrap_or_default(),
                    r.exit_code.map(|v| v.to_string()).unwrap_or_else(|| "null".into()),
                )
            })
            .collect();
        println!("[{}]", parts.join(","));
    } else {
        println!("ID                 STATUS     PID      EXIT     REPO");
        for r in rows {
            println!(
                "{:<18} {:<10} {:<8} {:<8} {}",
                r.id,
                r.status,
                r.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
                r.exit_code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "-".into()),
                r.repo,
            );
        }
    }
    Ok(())
}

fn run_agent_kills(limit: usize, json: bool) -> Result<()> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME not set"))?;
    let db_path = agent_runs_db::default_db_path(&home);
    if !db_path.exists() {
        if json {
            println!("[]");
        }
        return Ok(());
    }
    let conn = agent_runs_db::open(&db_path)?;
    let evs = agent_runs_db::list_kill_events(&conn, limit)?;
    if json {
        let parts: Vec<String> = evs
            .iter()
            .map(|e| {
                format!(
                    r#"{{"run_id":"{}","reason":"{}","pid":{},"detail":"{}"}}"#,
                    e.run_id,
                    e.reason,
                    e.pid
                        .map(|p| p.to_string())
                        .unwrap_or_else(|| "null".into()),
                    e.detail.replace('"', "\\\""),
                )
            })
            .collect();
        println!("[{}]", parts.join(","));
    } else {
        println!("RUN_ID             REASON     PID      DETAIL");
        for e in evs {
            println!(
                "{:<18} {:<10} {:<8} {}",
                e.run_id,
                e.reason,
                e.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
                e.detail,
            );
        }
    }
    Ok(())
}

fn run_serve(root: &Path, port: u16, bind: &str, no_open: bool, init: bool) -> Result<()> {
    let bind: std::net::IpAddr = bind
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid --bind address '{bind}': {e}"))?;
    if !bind.is_loopback() {
        eprintln!(
            "warning: serving on {bind} (non-loopback). The viewer is \
             unauthenticated and exposes architecture — only do this on \
             a trusted network."
        );
    }
    crabcc_viz::serve(crabcc_viz::Config {
        bind,
        port,
        root: root.to_path_buf(),
        no_open,
        init,
    })
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

/// Human-friendly name for a `Cmd` variant — used by the structured
/// `command: enter` log line so per-command timing can be aggregated
/// without re-deriving the name from the noisy `Debug` repr.
fn cmd_name_for_log(c: &Cmd) -> &'static str {
    match c {
        Cmd::Index => "index",
        Cmd::Refresh { .. } => "refresh",
        Cmd::Watch { .. } => "watch",
        Cmd::Sym { .. } => "sym",
        Cmd::Refs { .. } => "refs",
        Cmd::Callers { .. } => "callers",
        Cmd::Outline { .. } => "outline",
        Cmd::Files { .. } => "files",
        Cmd::Fuzzy { .. } => "fuzzy",
        Cmd::Prefix { .. } => "prefix",
        Cmd::Grep { .. } => "grep",
        Cmd::FtsRebuild => "fts-rebuild",
        Cmd::Track { .. } => "track",
        Cmd::Memory { .. } => "memory",
        Cmd::Graph { .. } => "graph",
        Cmd::Upgrade { .. } => "upgrade",
        Cmd::Compress { .. } => "compress",
        Cmd::Completions { .. } => "completions",
        Cmd::Info { .. } => "info",
        Cmd::Openapi => "openapi",
        Cmd::Go => "go",
        Cmd::Agent { .. } => "agent",
        Cmd::AgentLs { .. } => "agent-ls",
        Cmd::AgentGuard { .. } => "agent-guard",
        Cmd::AgentKills { .. } => "agent-kills",
        Cmd::Serve { .. } => "serve",
        Cmd::InstallClaude { .. } => "install-claude",
        Cmd::OllamaStack { .. } => "ollama-stack",
        Cmd::Doctor { .. } => "doctor",
    }
}

/// Dispatches `crabcc ollama-stack <op>` — pure operator surface.
/// All output is JSON for machine consumers (issue #105 / #107). Errors
/// from the driver bubble up via `?` and surface as anyhow chains.
fn run_ollama_stack(op: &OllamaStackOp) -> Result<()> {
    use crabcc_core::ollama_stack as ols;
    let opts = ols::Options::new();
    match op {
        OllamaStackOp::Up => {
            let r = ols::up(&opts)?;
            println!("{}", sonic_rs::to_string(&r)?);
        }
        OllamaStackOp::Down { volumes } => {
            let stopped = ols::down(&opts, *volumes)?;
            println!(
                "{}",
                sonic_rs::to_string(&serde_json::json!({ "stopped": stopped }))?
            );
        }
        OllamaStackOp::Status => {
            let containers = ols::status(&opts)?;
            println!("{}", sonic_rs::to_string(&containers)?);
        }
        OllamaStackOp::Logs { service, tail } => {
            let body = ols::logs(&opts, service.as_deref(), *tail)?;
            // logs are intentionally NOT JSON-wrapped — passthrough so
            // tools like `less` and `grep` work as expected.
            print!("{body}");
        }
        OllamaStackOp::Pull => {
            ols::pull(&opts)?;
            println!(
                "{}",
                sonic_rs::to_string(&serde_json::json!({ "ok": true }))?
            );
        }
    }
    Ok(())
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

/// Issue #74 — true if stderr is attached to a terminal. Skip the
/// "low-level surface" hint in pipelines / CI / mcp.
#[cfg(unix)]
fn atty_stderr() -> bool {
    // SAFETY: `isatty` only reads the fd's metadata; no aliasing.
    unsafe { libc::isatty(libc::STDERR_FILENO) == 1 }
}

#[cfg(not(unix))]
fn atty_stderr() -> bool {
    true
}

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
    with_stack: bool,
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
        // --check is read-only; --with-stack still pulls fresh image
        // metadata via `docker compose pull` so the operator sees what
        // *would* change without us mutating local state.
        if with_stack {
            run_upgrade_stack(false, text)?;
        }
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

    if with_stack {
        // pull-only when --apply is absent; pull then re-up when --apply
        // is set. Re-up recreates only services whose image digest
        // changed, so it's safe to call against an already-running stack.
        run_upgrade_stack(apply, text)?;
    }
    Ok(())
}

/// Issue #105 — `crabcc upgrade --with-stack` body. Pulls upstream
/// images for the bundled Compose stack and (when `recreate=true`)
/// re-ups so changed digests get picked up.
fn run_upgrade_stack(recreate: bool, text: bool) -> Result<()> {
    use anyhow::Context;
    use crabcc_core::ollama_stack as ols;
    ols::check_docker()?;
    let opts = ols::Options::new();
    if text {
        eprintln!("  → docker compose pull");
    }
    ols::pull(&opts)
        .context("ollama-stack pull failed. Run `crabcc doctor stack` for diagnostics.")?;
    if recreate {
        if text {
            eprintln!("  → docker compose up -d --wait");
        }
        let up = ols::up(&opts)
            .context("ollama-stack re-up failed. Run `crabcc doctor stack` to inspect health.")?;
        if text {
            eprintln!(
                "  → stack ready: {} services in {} ms",
                up.services_healthy.len(),
                up.duration_ms
            );
        } else {
            println!("{}", sonic_rs::to_string(&up)?);
        }
    } else if text {
        eprintln!("  (pass --apply to also `compose up -d --wait` and pick up new images)");
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
