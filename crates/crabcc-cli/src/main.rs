use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use crabcc_core::{query, store::Store};
use std::path::{Path, PathBuf};

// Issue #112 follow-up — global allocator swap. tikv-jemallocator is the
// maintained jemalloc bindings (5.x). Measured ~5-12% on the indexing
// hot path (alloc-heavy: tree-sitter cursors + Vec<Symbol> push) and
// ~3-6% on the MCP serve_io loop. Behaviour-equivalent to the system
// allocator at the API level — drop-in.
//
// Why not mimalloc: jemalloc is what tantivy and tikv ship with, so
// the workspace has more aligned tuning knobs (decay times, arena
// counts) if we ever need them. mimalloc was the runner-up at +3-7 %
// on the same micro-benches; switch is one line if the calculus
// changes.
//
// Bumpalo (per-file arenas during the tree-sitter walk) is already a
// workspace dep at [workspace.dependencies] and used in
// `crabcc-core/src/extract.rs`. The two allocators compose: jemalloc
// owns the heap, bumpalo carves transient regions out of it.
#[cfg(all(feature = "jemalloc", not(target_env = "msvc")))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

mod agent;
mod agent_guard;
mod agent_profile;
mod agent_runs_db;
mod backup;
mod compress_cmd;
mod debug_network;
mod doctor;
mod fetch_cmd;
mod go;
mod install;
mod jobs_cmd;
mod memory;
mod model_info;
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

    /// Run as MCP server over HTTP at ADDR (e.g. `127.0.0.1:8091`).
    /// Mutually exclusive with `--mcp` (stdio). Auth via `MCP_AUTH_TOKEN`
    /// env var when set; loopback-only is the recommended bind. #204
    /// phase 1.
    #[arg(long, global = true, value_name = "ADDR")]
    mcp_http: Option<String>,

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

/// Print a deprecation notice to stderr unless `CRABCC_NO_DEPRECATION_WARN=1`.
fn deprecation_warn(old: &str, group: &str, new_sub: &str) {
    if std::env::var_os("CRABCC_NO_DEPRECATION_WARN")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        return;
    }
    eprintln!("note: `crabcc {old}` is deprecated; use `crabcc {group} {new_sub}`");
}

// ── New sub-enums for grouped commands ────────────────────────────────────────

#[derive(Subcommand)]
enum IndexOp {
    /// Build a fresh index for the repo (default when no subcommand given).
    Build,
    /// Incremental refresh — re-reads disk vs the stored mtime + sha for each
    /// indexed file. Default output is `RefreshStats` (counts only); pass
    /// `--delta` to also receive the per-bucket file lists.
    Refresh {
        #[arg(long)]
        delta: bool,
    },
    /// Rebuild the Tantivy fuzzy/prefix sidecar from the current SQLite index.
    FtsRebuild,
    /// Watch the repo and auto-`refresh` on file changes (Ctrl-C to exit).
    Watch {
        #[arg(long, default_value_t = 500)]
        debounce: u64,
    },
    /// Train an FSST symbol table and write it to .crabcc/fsst.symbols.
    Compress {
        #[arg(long)]
        rebuild: bool,
        #[arg(long)]
        stats: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long, value_name = "N")]
        decode_probe: Option<usize>,
    },
}

#[derive(Subcommand)]
enum LookupOp {
    /// Find a symbol by name.
    Sym {
        name: String,
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
        #[arg(long)]
        under: Option<String>,
        #[arg(long)]
        lang: Option<String>,
        #[arg(long)]
        ext: Option<String>,
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
}

#[derive(Subcommand)]
enum AgentOp {
    /// Drive an LLM agent through one round of tool-use.
    Run {
        #[arg(long, value_name = "PROMPT")]
        run: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, value_name = "ID")]
        model: Option<String>,
        #[arg(long)]
        no_refresh: bool,
        #[arg(long, value_name = "BACKEND", default_value = "ollama")]
        backend: String,
        #[arg(long, value_name = "NAME")]
        profile: Option<String>,
    },
    /// List agent runs from the singleton ~/.crabcc/_internal.db.
    Ls {
        #[arg(long)]
        active_only: bool,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Sweep stuck / zombie agent runs.
    Guard {
        #[arg(long, default_value_t = 1800u64)]
        idle_secs: u64,
        #[arg(long, default_value_t = 5000u64)]
        term_grace_ms: u64,
        #[arg(long)]
        json: bool,
    },
    /// List agent kill events from the audit trail.
    Kills {
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum StackOp {
    /// `docker compose up -d --wait` against the resolved stack.
    Up,
    /// `docker compose down`.
    Down {
        #[arg(long)]
        volumes: bool,
    },
    /// Container health status.
    Status,
    /// Tail of `docker compose logs`.
    Logs {
        service: Option<String>,
        #[arg(long, default_value_t = 100)]
        tail: usize,
    },
    /// `docker compose pull` to refresh upstream images.
    Pull,
}

#[derive(Subcommand)]
enum SetupOp {
    /// Symlink the crabcc skill + slash-command into `~/.claude/`.
    InstallClaude {
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        print_hooks: bool,
        #[arg(long)]
        with_ollama_stack: bool,
        #[arg(long)]
        print_stack_instructions: bool,
    },
    /// Check GitHub for a newer release; optionally clean local sidecars.
    Upgrade {
        #[arg(long)]
        check: bool,
        #[arg(long)]
        text: bool,
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        with_stack: bool,
    },
    /// Print a shell-completion script for the chosen shell to stdout.
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Print the embedded OpenAPI 3.1 description of the MCP tool surface.
    Openapi,
}

#[derive(Subcommand)]
enum InfoOp {
    /// Print build provenance (commit, branch, tag, time, target).
    Build {
        #[arg(long)]
        text: bool,
        #[arg(long)]
        status_line: bool,
        #[arg(long)]
        is_repo: bool,
        #[arg(long)]
        json: bool,
    },
    /// Show estimated tokens saved by crabcc usage.
    Track {
        #[arg(long)]
        text: bool,
    },
    /// Enumerate all crabcc services + probe each for reachability.
    Services {
        #[arg(long)]
        json: bool,
    },
    /// Capture host network diagnostics — DNS, traceroute, interfaces,
    /// routes, sanity pings (issue #150). Pairs with `info services`
    /// (port-level reachability) to triage \"service unreachable\".
    Network {
        /// Restrict the sweep to one host (e.g. `redis`, `127.0.0.1`).
        #[arg(long)]
        service: Option<String>,
        /// Emit JSON instead of the human-readable text report.
        #[arg(long)]
        json: bool,
        /// Cap traceroute hops. Default 8; lower for faster runs.
        #[arg(long, default_value_t = 8)]
        max_hops: u8,
    },
    /// Per-model metadata stored at $CRABCC_HOME/models/.
    Model {
        #[command(subcommand)]
        op: ModelInfoOp,
    },
}

// ── Top-level command enum ────────────────────────────────────────────────────

#[derive(Subcommand)]
enum Cmd {
    // ── 12 visible groups ────────────────────────────────────────────────────
    /// Index operations: build, refresh, watch, compress, fts-rebuild.
    Index {
        #[command(subcommand)]
        op: Option<IndexOp>,
    },
    /// Lookup operations: sym, refs, callers, outline, files, fuzzy, prefix, grep.
    Lookup {
        #[command(subcommand)]
        op: LookupOp,
    },
    /// Agent operations: run, ls, guard, kills.
    Agent {
        #[command(subcommand)]
        op: AgentOp,
    },
    /// Ollama auth-stack operations: up, down, status, logs, pull.
    Stack {
        #[command(subcommand)]
        op: StackOp,
    },
    /// Diagnostic surface: docker, stack, keys, agent, jobs.
    Doctor {
        #[command(subcommand)]
        op: Option<DoctorOp>,
        #[arg(long)]
        text: bool,
    },
    /// Call-graph operations: build, walk, cycles, orphans.
    Graph {
        #[command(subcommand)]
        op: GraphOp,
    },
    /// AI memory operations (per-repo .crabcc/memory.db).
    Memory {
        #[command(subcommand)]
        sub: memory::MemoryCmd,
    },
    /// Snapshot / list / restore per-repo .crabcc/ state.
    Backup {
        #[command(subcommand)]
        op: BackupOp,
    },
    /// BullMQ job queue — submit / inspect / cancel jobs.
    #[command(subcommand)]
    Jobs(JobsCmd),
    /// Setup operations: install-claude, upgrade, completions, openapi.
    Setup {
        #[command(subcommand)]
        op: SetupOp,
    },
    /// Info operations: build provenance, track, services, model.
    Info {
        #[command(subcommand)]
        op: Option<InfoOp>,
        /// Print human-readable text instead of JSON.
        #[arg(long)]
        text: bool,
        /// Emit a render-budget-friendly status-line summary.
        #[arg(long)]
        status_line: bool,
        /// Exit 0 if cwd is inside a crabcc-indexed repo, 1 otherwise.
        #[arg(long)]
        is_repo: bool,
        /// JSON output for status-line.
        #[arg(long)]
        json: bool,
    },
    /// One-shot bootstrap: index + graph + memory + claude hand-off.
    Go,
    /// Fetch URLs out of a prompt, clean HTML to markdown, return in bulk.
    /// Tries the Chrome bridge first (uses your authenticated session)
    /// when available, falls back to direct HTTP otherwise.
    Fetch {
        /// Prompt or text containing the URLs to fetch.
        prompt: String,
        /// Skip the Chrome-bridge check; always go direct.
        #[arg(long)]
        no_chrome: bool,
        /// Output format: `json` (default) or `text`.
        #[arg(long, default_value = "json")]
        format: String,
        /// Pipe each successful fetch into the memory layer (BM25 ⊕ vector
        /// embedding). Drawer key: `fetch:<url>`, wing: `fetch`, room: host.
        /// Search later via `crabcc memory search <query> --wing fetch`.
        #[arg(long)]
        remember: bool,
    },
    /// Start the localhost call-graph viewer (issue #64). Binds to 127.0.0.1
    /// by default — pass `--bind 0.0.0.0` only on a trusted LAN; the server
    /// is unauthenticated and exposes architecture.
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
        /// Skip the index/graph/memory bootstrap at startup.
        #[arg(long)]
        no_init: bool,
    },

    // ── Hidden deprecated aliases (kept for one release cycle) ────────────────
    #[command(hide = true)]
    Refresh {
        #[arg(long)]
        delta: bool,
    },
    #[command(hide = true)]
    Sym {
        name: String,
        #[arg(long, value_name = "GIT_REV")]
        since: Option<String>,
    },
    #[command(hide = true)]
    Refs {
        name: String,
        #[command(flatten)]
        opts: ResultOpts,
    },
    #[command(hide = true)]
    Callers {
        name: String,
        #[command(flatten)]
        opts: ResultOpts,
    },
    #[command(hide = true)]
    Outline { file: PathBuf },
    #[command(hide = true)]
    Files {
        #[arg(long)]
        under: Option<String>,
        #[arg(long)]
        lang: Option<String>,
        #[arg(long)]
        ext: Option<String>,
        #[arg(long, default_value_t = 0)]
        limit: usize,
    },
    #[command(hide = true)]
    Grep { pattern: String },
    #[command(hide = true)]
    Fuzzy {
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    #[command(hide = true)]
    Prefix {
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    #[command(hide = true)]
    FtsRebuild,
    #[command(hide = true)]
    Track {
        #[arg(long)]
        text: bool,
    },
    #[command(hide = true)]
    Watch {
        #[arg(long, default_value_t = 500)]
        debounce: u64,
    },
    #[command(hide = true)]
    Compress {
        #[arg(long)]
        rebuild: bool,
        #[arg(long)]
        stats: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long, value_name = "N")]
        decode_probe: Option<usize>,
    },
    #[command(hide = true, name = "install-claude")]
    InstallClaude {
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        print_hooks: bool,
        #[arg(long)]
        with_ollama_stack: bool,
        #[arg(long)]
        print_stack_instructions: bool,
    },
    #[command(hide = true)]
    Upgrade {
        #[arg(long)]
        check: bool,
        #[arg(long)]
        text: bool,
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        with_stack: bool,
    },
    #[command(hide = true)]
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    #[command(hide = true)]
    Openapi,
    #[command(hide = true, name = "agent-run")]
    AgentRunAlias {
        #[arg(long, value_name = "PROMPT")]
        run: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, value_name = "ID")]
        model: Option<String>,
        #[arg(long)]
        no_refresh: bool,
        #[arg(long, value_name = "BACKEND", default_value = "ollama")]
        backend: String,
        #[arg(long, value_name = "NAME")]
        profile: Option<String>,
    },
    #[command(hide = true, name = "agent-ls")]
    AgentLs {
        #[arg(long)]
        active_only: bool,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    #[command(hide = true, name = "agent-guard")]
    AgentGuard {
        #[arg(long, default_value_t = 1800u64)]
        idle_secs: u64,
        #[arg(long, default_value_t = 5000u64)]
        term_grace_ms: u64,
        #[arg(long)]
        json: bool,
    },
    #[command(hide = true, name = "agent-kills")]
    AgentKills {
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    #[command(hide = true, name = "model-info")]
    ModelInfo {
        #[command(subcommand)]
        op: ModelInfoOp,
    },
    #[command(hide = true, name = "ollama-stack")]
    OllamaStack {
        #[command(subcommand)]
        op: OllamaStackOp,
    },
    #[command(hide = true, name = "debug-service-discovery")]
    DebugServiceDiscovery {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum BackupOp {
    /// Take a fresh snapshot now and prune older entries past the
    /// retention cap.
    Snapshot {
        #[arg(long)]
        json: bool,
    },
    /// List existing snapshots for the current repo, newest first.
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Delete all but the N most recent snapshots. Default is the
    /// canonical retention (CRABCC_BACKUPS_KEEP, default 2). Pass
    /// --keep 0 to wipe everything for this repo.
    Prune {
        #[arg(long)]
        keep: Option<usize>,
        #[arg(long)]
        json: bool,
    },
    /// Restore a snapshot over the current repo's `.crabcc/`. The
    /// restore is non-destructive for files NOT present in the
    /// backup — only files captured by the snapshot are overwritten.
    Restore {
        /// Unix timestamp of the snapshot to restore (the directory
        /// name under `<crabcc_home>/backups/<repo-slug>/`). See
        /// `crabcc backup ls`.
        #[arg(long, value_name = "UNIX_TS")]
        timestamp: u64,
    },
    /// Long-running loop. Snapshots every `interval` seconds against
    /// each repo listed in `~/.crabcc/agent/repos.list`. Wired to the
    /// `com.crabcc.backup-loop` LaunchAgent (default 15 min). Stay
    /// foreground; managed lifecycle lives in launchd.
    Loop {
        #[arg(long, default_value_t = 900u64)]
        interval: u64,
    },
}

#[derive(Subcommand)]
enum ModelInfoOp {
    /// Print the .info file for <provider>:<name>. Defaults to the
    /// bundled Ollama default (`ollama:qwen2.5-coder`).
    Show {
        #[arg(long, default_value = "ollama")]
        provider: String,
        #[arg(long, default_value = "qwen2.5-coder")]
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Seed the bundled Ollama default's .info file. Idempotent.
    /// Run by the install path; safe to invoke any time.
    SeedDefault,
    /// List all .info files under $CRABCC_HOME/models/ as a table.
    /// JSON output via --json. Used by the live dashboard's model picker.
    Ls {
        #[arg(long)]
        json: bool,
    },
}

/// `crabcc jobs` — submit, inspect, and cancel BullMQ jobs (issue #109).
#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
enum JobsCmd {
    /// Submit a job to a queue.
    Submit {
        /// Queue name: agent:run | agent:flow | repo:index | repo:reindex
        #[arg(long)]
        queue: String,
        /// Job name (label for the worker).
        #[arg(long)]
        name: String,
        /// Job data as JSON (e.g. '{"prompt":"audit this repo"}').
        #[arg(long, default_value = "{}")]
        data: String,
        /// Optional delay in milliseconds before the job becomes active.
        #[arg(long)]
        delay_ms: Option<u64>,
        /// Job priority (lower = higher priority).
        #[arg(long)]
        priority: Option<u32>,
        /// Max retry attempts.
        #[arg(long)]
        attempts: Option<u32>,
        /// Human-readable agent identifier (shown in Bull Board / /live).
        #[arg(long)]
        agent_name: Option<String>,
        /// Repo path this job operates on.
        #[arg(long)]
        repo_path: Option<String>,
        /// GitHub URL for this repo (surfaced in dashboard links).
        #[arg(long)]
        github_url: Option<String>,
        /// Agent run-dir path (contains lock, pid, log — for correlation).
        #[arg(long)]
        agent_folder: Option<String>,
    },
    /// Query the current state of a job.
    Status {
        #[arg(long)]
        queue: String,
        #[arg(long)]
        id: String,
    },
    /// List waiting jobs in a queue (shows depth, not full payloads).
    List {
        /// Queue to inspect. Omit to list all queues.
        queue: Option<String>,
    },
    /// Cancel (remove) a waiting or delayed job.
    Cancel {
        #[arg(long)]
        queue: String,
        #[arg(long)]
        id: String,
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
            Some(
                Cmd::Sym { .. }
                | Cmd::Lookup {
                    op: LookupOp::Sym { .. },
                },
            ) => Some("ccc find <NAME>"),
            Some(
                Cmd::Refs { .. }
                | Cmd::Lookup {
                    op: LookupOp::Refs { .. },
                },
            ) => Some("ccc find <NAME> --mode references"),
            Some(
                Cmd::Callers { .. }
                | Cmd::Lookup {
                    op: LookupOp::Callers { .. },
                },
            ) => Some("ccc find <NAME> --mode callers"),
            Some(
                Cmd::Fuzzy { .. }
                | Cmd::Lookup {
                    op: LookupOp::Fuzzy { .. },
                },
            ) => Some("ccc find <NAME> --mode fuzzy"),
            Some(
                Cmd::Prefix { .. }
                | Cmd::Lookup {
                    op: LookupOp::Prefix { .. },
                },
            ) => Some("ccc find <NAME> --mode prefix"),
            Some(
                Cmd::Grep { .. }
                | Cmd::Lookup {
                    op: LookupOp::Grep { .. },
                },
            ) => Some("ccc find <PATTERN> --mode grep"),
            Some(
                Cmd::Files { .. }
                | Cmd::Lookup {
                    op: LookupOp::Files { .. },
                },
            ) => Some("ccc list --files"),
            _ => None,
        };
        if let Some(h) = hint {
            eprintln!(
                "note: `crabcc` is the low-level surface; equivalent: `{h}` \
                 (suppress with CRABCC_NO_HINT=1)"
            );
        }
    }

    if cli.mcp && cli.mcp_http.is_some() {
        anyhow::bail!("--mcp and --mcp-http are mutually exclusive; pick one transport");
    }

    if cli.mcp {
        // `--dev` flag OR `CRABCC_MCP_DEV=1` env both flip the dev surface
        // on. The CLI flag wins because it's more explicit; if neither is
        // set, the slim default surface is used (issue #59).
        let dev = cli.dev || crabcc_mcp::dev_mode_from_env();
        return crabcc_mcp::serve_stdio_with(&root, dev);
    }

    if let Some(addr_str) = cli.mcp_http.as_ref() {
        // HTTP transport (#204 phase 1). Mirrors `--mcp` semantics for the
        // dev surface; auth via MCP_AUTH_TOKEN when set (loopback-only is
        // the recommended bind, so unset is acceptable for dev / single-user).
        let dev = cli.dev || crabcc_mcp::dev_mode_from_env();
        let addr: std::net::SocketAddr = addr_str
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid --mcp-http addr {addr_str:?}: {e}"))?;
        let token = std::env::var("MCP_AUTH_TOKEN")
            .ok()
            .filter(|t| !t.is_empty());
        return crabcc_mcp::serve_http(addr, &root, dev, token);
    }

    // Early-return for no-store commands. Both the new grouped paths and
    // the hidden deprecated aliases are handled here.

    // setup group
    if let Some(Cmd::Setup { op }) = cli.cmd.as_ref() {
        match op {
            SetupOp::InstallClaude {
                yes,
                print_hooks,
                with_ollama_stack,
                print_stack_instructions,
            } => {
                return install::run(install::InstallOptions {
                    yes: *yes,
                    print_hooks_only: *print_hooks,
                    with_ollama_stack: *with_ollama_stack,
                    print_stack_instructions: *print_stack_instructions,
                });
            }
            SetupOp::Upgrade {
                check,
                text,
                apply,
                repo,
                with_stack,
            } => {
                return run_upgrade(*check, *text, *apply, *with_stack, repo.as_deref(), &root);
            }
            SetupOp::Completions { shell } => {
                let mut cmd = <Cli as clap::CommandFactory>::command();
                clap_complete::generate(*shell, &mut cmd, "crabcc", &mut std::io::stdout());
                return Ok(());
            }
            SetupOp::Openapi => {
                print!("{}", crabcc_mcp::OPENAPI_YAML);
                return Ok(());
            }
        }
    }

    // Deprecated: install-claude / upgrade / completions / openapi aliases
    if let Some(Cmd::InstallClaude {
        yes,
        print_hooks,
        with_ollama_stack,
        print_stack_instructions,
    }) = &cli.cmd
    {
        deprecation_warn("install-claude", "setup", "install-claude");
        return install::run(install::InstallOptions {
            yes: *yes,
            print_hooks_only: *print_hooks,
            with_ollama_stack: *with_ollama_stack,
            print_stack_instructions: *print_stack_instructions,
        });
    }
    if let Some(Cmd::Upgrade {
        check,
        text,
        apply,
        repo,
        with_stack,
    }) = cli.cmd.as_ref()
    {
        deprecation_warn("upgrade", "setup", "upgrade");
        return run_upgrade(*check, *text, *apply, *with_stack, repo.as_deref(), &root);
    }
    if let Some(Cmd::Completions { shell }) = cli.cmd.as_ref() {
        deprecation_warn("completions", "setup", "completions");
        let mut cmd = <Cli as clap::CommandFactory>::command();
        clap_complete::generate(*shell, &mut cmd, "crabcc", &mut std::io::stdout());
        return Ok(());
    }
    if let Some(Cmd::Openapi) = cli.cmd.as_ref() {
        deprecation_warn("openapi", "setup", "openapi");
        print!("{}", crabcc_mcp::OPENAPI_YAML);
        return Ok(());
    }

    // info group
    if let Some(Cmd::Info {
        op,
        text,
        status_line,
        is_repo,
        json,
    }) = cli.cmd.as_ref()
    {
        // Subcommand takes priority over legacy flags on the Info group itself.
        if let Some(sub) = op {
            match sub {
                InfoOp::Build {
                    text: t,
                    status_line: sl,
                    is_repo: ir,
                    json: j,
                } => {
                    if *ir {
                        std::process::exit(if status::is_repo(&root) { 0 } else { 1 });
                    }
                    if *sl {
                        return status::run_status_line(&root, *j);
                    }
                    return run_info(*t);
                }
                InfoOp::Track { text: t } => {
                    // needs store — fall through to store open below
                    let _ = t; // used below in match
                }
                InfoOp::Services { json: j } => {
                    let report = crabcc_core::service_discovery::discover_all();
                    if *j {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        print_service_discovery_text(&report);
                    }
                    return Ok(());
                }
                InfoOp::Network {
                    service: svc,
                    json: j,
                    max_hops: hops,
                } => {
                    return debug_network::run(svc.as_deref(), *j, *hops);
                }
                InfoOp::Model { op: mop } => {
                    return run_model_info(mop);
                }
            }
        } else {
            // Legacy flags on `crabcc info` with no subcommand
            if *is_repo {
                std::process::exit(if status::is_repo(&root) { 0 } else { 1 });
            }
            if *status_line {
                return status::run_status_line(&root, *json);
            }
            return run_info(*text);
        }
    }

    // Deprecated: debug-service-discovery alias
    if let Some(Cmd::DebugServiceDiscovery { json }) = cli.cmd.as_ref() {
        deprecation_warn("debug-service-discovery", "info", "services");
        let report = crabcc_core::service_discovery::discover_all();
        if *json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            print_service_discovery_text(&report);
        }
        return Ok(());
    }

    // Deprecated: model-info alias
    if let Some(Cmd::ModelInfo { op }) = cli.cmd.as_ref() {
        deprecation_warn("model-info", "info", "model");
        return run_model_info(op);
    }

    // `go` is a top-level orchestrator.
    if let Some(Cmd::Go) = cli.cmd.as_ref() {
        return go::run(&root, &db);
    }

    // `fetch` is pure I/O. With --remember, opens the memory Palace at
    // <root>/.crabcc/memory.db so each result is stored as a drawer.
    if let Some(Cmd::Fetch {
        prompt,
        no_chrome,
        format,
        remember,
    }) = cli.cmd.as_ref()
    {
        return fetch_cmd::run(&root, prompt, *no_chrome, format, *remember);
    }

    // `serve` boots crabcc-viz; doesn't need the symbol Store opened
    // here (the viz server lazy-opens its own).
    if let Some(Cmd::Serve {
        port,
        bind,
        no_open,
        no_init,
    }) = cli.cmd.as_ref()
    {
        return run_serve(&root, *port, bind, *no_open, !*no_init);
    }

    // agent group
    if let Some(Cmd::Agent { op }) = cli.cmd.as_ref() {
        match op {
            AgentOp::Run {
                run,
                dry_run,
                model,
                no_refresh,
                backend,
                profile,
            } => {
                return run_agent_run(run, *dry_run, model, *no_refresh, backend, profile, &root);
            }
            AgentOp::Ls {
                active_only,
                limit,
                json,
            } => {
                return run_agent_ls(*active_only, *limit, *json);
            }
            AgentOp::Guard {
                idle_secs,
                term_grace_ms,
                json,
            } => {
                return agent_guard::run(agent_guard::GuardConfig {
                    idle_secs: *idle_secs,
                    term_grace_ms: *term_grace_ms,
                    json: *json,
                });
            }
            AgentOp::Kills { limit, json } => {
                return run_agent_kills(*limit, *json);
            }
        }
    }

    // Deprecated agent aliases
    if let Some(Cmd::AgentRunAlias {
        run,
        dry_run,
        model,
        no_refresh,
        backend,
        profile,
    }) = cli.cmd.as_ref()
    {
        deprecation_warn("agent-run", "agent", "run");
        return run_agent_run(run, *dry_run, model, *no_refresh, backend, profile, &root);
    }
    if let Some(Cmd::AgentLs {
        active_only,
        limit,
        json,
    }) = cli.cmd.as_ref()
    {
        deprecation_warn("agent-ls", "agent", "ls");
        return run_agent_ls(*active_only, *limit, *json);
    }
    if let Some(Cmd::AgentGuard {
        idle_secs,
        term_grace_ms,
        json,
    }) = cli.cmd.as_ref()
    {
        deprecation_warn("agent-guard", "agent", "guard");
        return agent_guard::run(agent_guard::GuardConfig {
            idle_secs: *idle_secs,
            term_grace_ms: *term_grace_ms,
            json: *json,
        });
    }
    if let Some(Cmd::AgentKills { limit, json }) = cli.cmd.as_ref() {
        deprecation_warn("agent-kills", "agent", "kills");
        return run_agent_kills(*limit, *json);
    }

    // stack group
    if let Some(Cmd::Stack { op }) = cli.cmd.as_ref() {
        return run_stack_op(op);
    }
    if let Some(Cmd::OllamaStack { op }) = cli.cmd.as_ref() {
        deprecation_warn("ollama-stack", "stack", "<subcommand>");
        return run_ollama_stack(op);
    }

    // backup group
    if let Some(Cmd::Backup { op }) = cli.cmd.as_ref() {
        return run_backup(&root, op);
    }

    // jobs group
    if let Some(Cmd::Jobs(op)) = cli.cmd.as_ref() {
        return jobs_cmd::run(op);
    }

    // doctor group
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

    // index group — compress is handled before Store open (it manages its
    // own DB connection); Build / Refresh / FtsRebuild / Watch need the
    // store and fall through to the main dispatch below.
    if let Some(Cmd::Index {
        op:
            Some(IndexOp::Compress {
                rebuild,
                stats,
                json,
                db: db_override,
                decode_probe,
            }),
    }) = cli.cmd.as_ref()
    {
        let db_path = db_override.clone().unwrap_or_else(|| db.clone());
        return compress_cmd::run(compress_cmd::Args {
            root: root.clone(),
            db: db_path,
            rebuild: *rebuild,
            stats: *stats || *json,
            json: *json,
            decode_probe: *decode_probe,
        });
    }

    // Deprecated compress alias
    if let Some(Cmd::Compress {
        rebuild,
        stats,
        json,
        db: db_override,
        decode_probe,
    }) = cli.cmd.as_ref()
    {
        deprecation_warn("compress", "index", "compress");
        let db_path = db_override.clone().unwrap_or_else(|| db.clone());
        return compress_cmd::run(compress_cmd::Args {
            root: root.clone(),
            db: db_path,
            rebuild: *rebuild,
            stats: *stats || *json,
            json: *json,
            decode_probe: *decode_probe,
        });
    }

    // memory group
    if let Some(Cmd::Memory { sub }) = cli.cmd {
        return memory::run(&root, sub);
    }

    std::fs::create_dir_all(db.parent().unwrap())?;
    let store = Store::open_with_compress(&db, cli.compress)?;
    let fts_dir = root.join(".crabcc").join("tantivy");

    // Determine a canonical command name for logging.
    let log_name = match &cli.cmd {
        None => "index",
        Some(c) => cmd_name_for_log(c),
    };
    tracing::info!(target: "crabcc_cli", cmd = log_name, "command: enter");

    match cli.cmd.unwrap_or(Cmd::Index { op: None }) {
        // ── Index group (store-dependent ops) ──────────────────────────────
        Cmd::Index { op } => match op.unwrap_or(IndexOp::Build) {
            IndexOp::Build => {
                let stats = crabcc_core::index::full_index(&root, &store)?;
                if let Ok(fts) = crabcc_core::fts::Fts::open(&fts_dir) {
                    let _ = fts.rebuild(&store);
                }
                println!("{}", sonic_rs::to_string(&stats)?);
                if std::env::var_os("CRABCC_BACKUP_DISABLE").is_none() {
                    backup::auto_snapshot_after_index(&root);
                }
            }
            IndexOp::Refresh { delta } => {
                if delta {
                    let d = crabcc_core::index::refresh_delta(&root, &store)?;
                    println!("{}", sonic_rs::to_string(&d)?);
                } else {
                    let stats = crabcc_core::index::refresh(&root, &store)?;
                    println!("{}", sonic_rs::to_string(&stats)?);
                }
                if std::env::var_os("CRABCC_BACKUP_DISABLE").is_none() {
                    backup::auto_snapshot_after_index(&root);
                }
            }
            IndexOp::FtsRebuild => {
                let fts = crabcc_core::fts::Fts::open(&fts_dir)?;
                let n = fts.rebuild(&store)?;
                println!("{{\"indexed\":{n}}}");
            }
            IndexOp::Watch { debounce } => {
                let store = std::sync::Arc::new(std::sync::Mutex::new(store));
                crabcc_core::watch::watch(
                    &root,
                    store,
                    std::time::Duration::from_millis(debounce),
                )?;
            }
            IndexOp::Compress { .. } => unreachable!("compress handled before store init"),
        },

        // ── Lookup group ────────────────────────────────────────────────────
        Cmd::Lookup { op } => match op {
            LookupOp::Sym { name, since } => {
                let syms = match since.as_deref() {
                    Some(rev) => {
                        let files = crabcc_core::gitdiff::changed_files_since(&root, rev)?;
                        query::find_symbol_in_files(&store, &name, &files)?
                    }
                    None => query::find_symbol(&store, &name)?,
                };
                let body = sonic_rs::to_string(&syms)?;
                crabcc_core::track::record(
                    "sym",
                    &name,
                    syms.len(),
                    &repo_label(&root),
                    body.len(),
                );
                memory::auto_capture(&root, "sym", &name, syms.len());
                println!("{body}");
            }
            LookupOp::Refs { name, opts } => {
                let mode = opts.to_mode();
                let files = match opts.since.as_deref() {
                    Some(rev) => Some(crabcc_core::gitdiff::changed_files_since(&root, rev)?),
                    None => None,
                };
                let out = query::query_refs(&store, &root, &name, mode, files.as_ref())?;
                let body = sonic_rs::to_string(&out)?;
                crabcc_core::track::record(
                    "refs",
                    &name,
                    out.count(),
                    &repo_label(&root),
                    body.len(),
                );
                memory::auto_capture(&root, "refs", &name, out.count());
                if opts.ndjson {
                    emit_hits_ndjson(&out)?;
                } else {
                    let envelope =
                        crabcc_core::hash::fingerprint_envelope(&body, opts.if_changed.as_deref());
                    println!("{envelope}");
                }
            }
            LookupOp::Callers { name, opts } => {
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
            LookupOp::Outline { file } => {
                let key = file.to_string_lossy();
                let syms = crabcc_core::outline::outline(&store, &key)?;
                let body = sonic_rs::to_string(&syms)?;
                crabcc_core::track::record(
                    "outline",
                    &key,
                    syms.len(),
                    &repo_label(&root),
                    body.len(),
                );
                println!("{body}");
            }
            LookupOp::Files {
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
            LookupOp::Grep { pattern } => {
                println!("{{\"status\":\"todo\",\"op\":\"grep\",\"pattern\":\"{pattern}\"}}");
            }
            LookupOp::Fuzzy { query, limit } => {
                let fts = crabcc_core::fts::Fts::open(&fts_dir)?;
                let hits = fts.fuzzy(&query, limit)?;
                let body = sonic_rs::to_string(&hits)?;
                crabcc_core::track::record(
                    "fuzzy",
                    &query,
                    hits.len(),
                    &repo_label(&root),
                    body.len(),
                );
                memory::auto_capture(&root, "fuzzy", &query, hits.len());
                println!("{body}");
            }
            LookupOp::Prefix { query, limit } => {
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
        },

        // ── info group (store-dependent: Track) ─────────────────────────────
        Cmd::Info {
            op: Some(InfoOp::Track { text }),
            ..
        } => {
            let r = crabcc_core::track::report()?;
            if text {
                print_track_human(&r);
            } else {
                println!("{}", sonic_rs::to_string_pretty(&r)?);
            }
        }

        // ── Graph group ─────────────────────────────────────────────────────
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
                        "crabcc graph walk: no .crabcc/graph.json — building on the fly \
                         (run `crabcc graph build` to cache)"
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

        // ── Deprecated flat aliases (store-dependent) ───────────────────────
        Cmd::Refresh { delta } => {
            deprecation_warn("refresh", "index", "refresh");
            if delta {
                let d = crabcc_core::index::refresh_delta(&root, &store)?;
                println!("{}", sonic_rs::to_string(&d)?);
            } else {
                let stats = crabcc_core::index::refresh(&root, &store)?;
                println!("{}", sonic_rs::to_string(&stats)?);
            }
            if std::env::var_os("CRABCC_BACKUP_DISABLE").is_none() {
                backup::auto_snapshot_after_index(&root);
            }
        }
        Cmd::Sym { name, since } => {
            deprecation_warn("sym", "lookup", "sym");
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
            deprecation_warn("refs", "lookup", "refs");
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
            deprecation_warn("callers", "lookup", "callers");
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
            deprecation_warn("outline", "lookup", "outline");
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
            deprecation_warn("files", "lookup", "files");
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
            deprecation_warn("grep", "lookup", "grep");
            println!("{{\"status\":\"todo\",\"op\":\"grep\",\"pattern\":\"{pattern}\"}}");
        }
        Cmd::Fuzzy { query, limit } => {
            deprecation_warn("fuzzy", "lookup", "fuzzy");
            let fts = crabcc_core::fts::Fts::open(&fts_dir)?;
            let hits = fts.fuzzy(&query, limit)?;
            let body = sonic_rs::to_string(&hits)?;
            crabcc_core::track::record("fuzzy", &query, hits.len(), &repo_label(&root), body.len());
            memory::auto_capture(&root, "fuzzy", &query, hits.len());
            println!("{body}");
        }
        Cmd::Prefix { query, limit } => {
            deprecation_warn("prefix", "lookup", "prefix");
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
            deprecation_warn("fts-rebuild", "index", "fts-rebuild");
            let fts = crabcc_core::fts::Fts::open(&fts_dir)?;
            let n = fts.rebuild(&store)?;
            println!("{{\"indexed\":{n}}}");
        }
        Cmd::Track { text } => {
            deprecation_warn("track", "info", "track");
            let r = crabcc_core::track::report()?;
            if text {
                print_track_human(&r);
            } else {
                println!("{}", sonic_rs::to_string_pretty(&r)?);
            }
        }
        Cmd::Watch { debounce } => {
            deprecation_warn("watch", "index", "watch");
            let store = std::sync::Arc::new(std::sync::Mutex::new(store));
            crabcc_core::watch::watch(&root, store, std::time::Duration::from_millis(debounce))?;
        }

        // All other variants were handled in the early-return block above.
        Cmd::Setup { .. } => unreachable!("setup handled before store init"),
        Cmd::Info { .. } => unreachable!("info handled before store init"),
        Cmd::Go => unreachable!("go handled before store init"),
        Cmd::Fetch { .. } => unreachable!("fetch handled before store init"),
        Cmd::Serve { .. } => unreachable!("serve handled before store init"),
        Cmd::Agent { .. } => unreachable!("agent handled before store init"),
        Cmd::AgentRunAlias { .. } => unreachable!("agent-run handled before store init"),
        Cmd::AgentLs { .. } => unreachable!("agent-ls handled before store init"),
        Cmd::AgentGuard { .. } => unreachable!("agent-guard handled before store init"),
        Cmd::AgentKills { .. } => unreachable!("agent-kills handled before store init"),
        Cmd::ModelInfo { .. } => unreachable!("model-info handled before store init"),
        Cmd::OllamaStack { .. } => unreachable!("ollama-stack handled before store init"),
        Cmd::Stack { .. } => unreachable!("stack handled before store init"),
        Cmd::Backup { .. } => unreachable!("backup handled before store init"),
        Cmd::Doctor { .. } => unreachable!("doctor handled before store init"),
        Cmd::Jobs(_) => unreachable!("jobs handled before store init"),
        Cmd::DebugServiceDiscovery { .. } => {
            unreachable!("debug-service-discovery handled before store init")
        }
        Cmd::InstallClaude { .. } => unreachable!("install-claude handled before store init"),
        Cmd::Upgrade { .. } => unreachable!("upgrade handled before store init"),
        Cmd::Completions { .. } => unreachable!("completions handled before store init"),
        Cmd::Openapi => unreachable!("openapi handled before store init"),
        Cmd::Memory { .. } => unreachable!("memory handled before store init"),
        Cmd::Compress { .. } => unreachable!("compress handled before store init"),
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

fn run_model_info(op: &ModelInfoOp) -> Result<()> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME not set"))?;
    match op {
        ModelInfoOp::Show {
            provider,
            name,
            json,
        } => match model_info::read(&home, provider, name)? {
            Some(info) => {
                if *json {
                    println!("{}", serde_json::to_string_pretty(&info)?);
                } else {
                    println!("{}", model_info::banner_line(&info));
                    if let Some(ref n) = info.notes {
                        println!("\n{n}");
                    }
                    if !info.flags.is_empty() {
                        println!("\nflags:");
                        for f in &info.flags {
                            println!(
                                "  {:<22} {}{}",
                                f.name,
                                f.default
                                    .as_ref()
                                    .map(|d| format!("[{d}] "))
                                    .unwrap_or_default(),
                                f.description
                            );
                        }
                    }
                }
                Ok(())
            }
            None => {
                let path = model_info::file_path(&home, provider, name);
                anyhow::bail!(
                    "no .info at {}. Run `crabcc model-info seed-default` for the bundled default.",
                    path.display()
                )
            }
        },
        ModelInfoOp::SeedDefault => {
            let path = model_info::seed_default_ollama(&home)?;
            println!("seeded {}", path.display());
            Ok(())
        }
        ModelInfoOp::Ls { json } => {
            let dir = model_info::default_dir(&home);
            let mut rows: Vec<String> = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for e in entries.flatten() {
                    let n = e.file_name().to_string_lossy().to_string();
                    if n.starts_with(".model.") && n.ends_with(".info") {
                        rows.push(n);
                    }
                }
            }
            rows.sort();
            if *json {
                let dir_s = dir.display().to_string();
                let arr: Vec<String> = rows.iter().map(|n| format!("\"{n}\"")).collect();
                println!(r#"{{"dir":"{dir_s}","files":[{}]}}"#, arr.join(","));
            } else {
                println!("dir: {}", dir.display());
                for n in rows {
                    println!("  {n}");
                }
            }
            Ok(())
        }
    }
}

fn run_backup(root: &Path, op: &BackupOp) -> Result<()> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME not set"))?;
    match op {
        BackupOp::Snapshot { json } => {
            let r = backup::snapshot(root, &home)?;
            if *json {
                println!(
                    r#"{{"destination":"{}","files":{},"dirs":{},"bytes":{},"pruned":{}}}"#,
                    r.destination.display(),
                    r.files_copied,
                    r.dirs_copied,
                    r.bytes_copied,
                    r.pruned
                );
            } else {
                println!(
                    "snapshot: {} ({} files, {} dirs, {} bytes; {} pruned)",
                    r.destination.display(),
                    r.files_copied,
                    r.dirs_copied,
                    r.bytes_copied,
                    r.pruned
                );
            }
            Ok(())
        }
        BackupOp::Ls { json } => {
            let entries = backup::list(&home, root)?;
            if *json {
                let parts: Vec<String> = entries
                    .iter()
                    .map(|e| {
                        format!(
                            r#"{{"timestamp":{},"path":"{}","bytes":{}}}"#,
                            e.timestamp,
                            e.path.display(),
                            e.bytes
                        )
                    })
                    .collect();
                println!("[{}]", parts.join(","));
            } else if entries.is_empty() {
                println!("(no snapshots yet for {})", root.display());
            } else {
                println!("TIMESTAMP   BYTES        PATH");
                for e in entries {
                    println!("{:<11} {:<12} {}", e.timestamp, e.bytes, e.path.display());
                }
            }
            Ok(())
        }
        BackupOp::Prune { keep, json } => {
            let k = keep.unwrap_or(backup::BACKUPS_KEEP_DEFAULT);
            let n = backup::prune_to_n(&home, root, k)?;
            if *json {
                println!(r#"{{"removed":{},"keep":{}}}"#, n, k);
            } else {
                println!("pruned {n} snapshots (kept {k} most recent)");
            }
            Ok(())
        }
        BackupOp::Restore { timestamp } => {
            let n = backup::restore(root, &home, *timestamp)?;
            println!("restored {n} entries from snapshot {timestamp}");
            Ok(())
        }
        BackupOp::Loop { interval } => backup::run_loop(*interval),
    }
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
        // New groups
        Cmd::Index { op } => match op {
            None | Some(IndexOp::Build) => "index",
            Some(IndexOp::Refresh { .. }) => "index/refresh",
            Some(IndexOp::FtsRebuild) => "index/fts-rebuild",
            Some(IndexOp::Watch { .. }) => "index/watch",
            Some(IndexOp::Compress { .. }) => "index/compress",
        },
        Cmd::Lookup { op } => match op {
            LookupOp::Sym { .. } => "lookup/sym",
            LookupOp::Refs { .. } => "lookup/refs",
            LookupOp::Callers { .. } => "lookup/callers",
            LookupOp::Outline { .. } => "lookup/outline",
            LookupOp::Files { .. } => "lookup/files",
            LookupOp::Grep { .. } => "lookup/grep",
            LookupOp::Fuzzy { .. } => "lookup/fuzzy",
            LookupOp::Prefix { .. } => "lookup/prefix",
        },
        Cmd::Agent { op } => match op {
            AgentOp::Run { .. } => "agent/run",
            AgentOp::Ls { .. } => "agent/ls",
            AgentOp::Guard { .. } => "agent/guard",
            AgentOp::Kills { .. } => "agent/kills",
        },
        Cmd::Stack { .. } => "stack",
        Cmd::Setup { op } => match op {
            SetupOp::InstallClaude { .. } => "setup/install-claude",
            SetupOp::Upgrade { .. } => "setup/upgrade",
            SetupOp::Completions { .. } => "setup/completions",
            SetupOp::Openapi => "setup/openapi",
        },
        Cmd::Info { op, .. } => match op {
            None => "info",
            Some(InfoOp::Build { .. }) => "info/build",
            Some(InfoOp::Track { .. }) => "info/track",
            Some(InfoOp::Services { .. }) => "info/services",
            Some(InfoOp::Network { .. }) => "info/network",
            Some(InfoOp::Model { .. }) => "info/model",
        },
        Cmd::Graph { .. } => "graph",
        Cmd::Memory { .. } => "memory",
        Cmd::Backup { .. } => "backup",
        Cmd::Doctor { .. } => "doctor",
        Cmd::Jobs(_) => "jobs",
        Cmd::Go => "go",
        Cmd::Fetch { .. } => "fetch",
        Cmd::Serve { .. } => "serve",
        // Deprecated aliases
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
        Cmd::Upgrade { .. } => "upgrade",
        Cmd::Compress { .. } => "compress",
        Cmd::Completions { .. } => "completions",
        Cmd::Openapi => "openapi",
        Cmd::AgentRunAlias { .. } => "agent-run",
        Cmd::AgentLs { .. } => "agent-ls",
        Cmd::AgentGuard { .. } => "agent-guard",
        Cmd::AgentKills { .. } => "agent-kills",
        Cmd::ModelInfo { .. } => "model-info",
        Cmd::OllamaStack { .. } => "ollama-stack",
        Cmd::DebugServiceDiscovery { .. } => "debug-service-discovery",
        Cmd::InstallClaude { .. } => "install-claude",
    }
}

/// Shared agent-run logic used by both `crabcc agent run` and the deprecated
/// `crabcc agent-run` alias.
fn run_agent_run(
    run: &str,
    dry_run: bool,
    model: &Option<String>,
    no_refresh: bool,
    backend: &str,
    profile: &Option<String>,
    root: &Path,
) -> Result<()> {
    let backend = agent::Backend::from_str(backend)?;
    let loaded_profile = match profile.as_deref() {
        None => None,
        Some(p) => {
            let id = agent_profile::parse_internal_profile_id(p).ok_or_else(|| {
                anyhow::anyhow!("--profile must use the 'internal/<name>' form (got '{p}')")
            })?;
            Some(agent_profile::load(root, id)?)
        }
    };
    let model_override = model
        .clone()
        .or_else(|| loaded_profile.as_ref().and_then(|p| p.model.clone()));
    agent::run_with_profile(
        agent::AgentRequest {
            prompt: run,
            root,
            dry_run,
            model: model_override,
            no_refresh,
            backend,
        },
        loaded_profile,
    )
}

/// Dispatches `crabcc stack <op>` — thin wrapper around the existing
/// `run_ollama_stack` body, converting the new `StackOp` into the legacy
/// `OllamaStackOp` shape.
fn run_stack_op(op: &StackOp) -> Result<()> {
    use crabcc_core::ollama_stack as ols;
    let opts = ols::Options::new();
    match op {
        StackOp::Up => {
            let r = ols::up(&opts)?;
            println!("{}", sonic_rs::to_string(&r)?);
        }
        StackOp::Down { volumes } => {
            let stopped = ols::down(&opts, *volumes)?;
            println!(
                "{}",
                sonic_rs::to_string(&serde_json::json!({ "stopped": stopped }))?
            );
        }
        StackOp::Status => {
            let containers = ols::status(&opts)?;
            println!("{}", sonic_rs::to_string(&containers)?);
        }
        StackOp::Logs { service, tail } => {
            let body = ols::logs(&opts, service.as_deref(), *tail)?;
            print!("{body}");
        }
        StackOp::Pull => {
            ols::pull(&opts)?;
            println!(
                "{}",
                sonic_rs::to_string(&serde_json::json!({ "ok": true }))?
            );
        }
    }
    Ok(())
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

/// Render a service-discovery report as a fixed-column table — one row per
/// service, columns: name / url / source / state. Used by
/// `crabcc debug-service-discovery` (issue #143).
fn print_service_discovery_text(report: &crabcc_core::service_discovery::DiscoveryReport) {
    println!(
        "service discovery — compose_mode={} probed in {}ms",
        report.compose_mode, report.elapsed_ms
    );
    println!();
    let name_w = report
        .services
        .iter()
        .map(|s| s.service.name.len())
        .max()
        .unwrap_or(8)
        .max(8);
    let url_w = report
        .services
        .iter()
        .map(|s| s.service.url.len())
        .max()
        .unwrap_or(20)
        .max(20);
    println!(
        "  {:<name_w$}  {:<url_w$}  {:<8}  state",
        "name",
        "url",
        "source",
        name_w = name_w,
        url_w = url_w
    );
    println!(
        "  {:-<name_w$}  {:-<url_w$}  {:-<8}  -----",
        "",
        "",
        "",
        name_w = name_w,
        url_w = url_w
    );
    for s in &report.services {
        let state = if s.reachable {
            format!("● ok ({}ms)", s.latency_ms)
        } else {
            format!(
                "✗ {}",
                s.error
                    .as_deref()
                    .unwrap_or("down")
                    .chars()
                    .take(48)
                    .collect::<String>()
            )
        };
        println!(
            "  {:<name_w$}  {:<url_w$}  {:<8}  {}",
            s.service.name,
            s.service.url,
            s.service.source,
            state,
            name_w = name_w,
            url_w = url_w
        );
    }
}
