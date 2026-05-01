//! `crabcc agent --run <prompt>` — drive an LLM agent through one round
//! of tool-use against the crabcc MCP surface.
//!
//! The runtime sits behind an [`AgentRuntime`] trait so we can swap the
//! current-day "exec claude as a subprocess on the host" implementation
//! for a sandboxed one (issue #62, v3.0). The default keeps trust scoped
//! exactly like `crabcc go` already does — agent processes run with the
//! invoking user's privileges, full filesystem + network access. The
//! sandbox impl will reduce that to "this temp dir + crabcc MCP socket
//! only" once microsandbox (or an alternative) stabilizes; see
//! `install/agent-runtime.md` for the v3.0 plan.
//!
//! Each run gets its own state directory at `~/.crabcc/agents/<id>/`:
//!
//! ```text
//! ~/.crabcc/agents/<id>/
//!   lock         empty file present while the run is in flight
//!   pid          PID of the spawned agent process
//!   log          tee'd stdout+stderr stream (tail -f from another shell)
//!   meta.json    {id, started_ts, prompt_preview, model, root, runtime}
//! ```
//!
//! The IDs are 16 hex chars sourced from `/dev/urandom` (collision-resistant
//! enough for single-user developer use; if /dev/urandom is unavailable we
//! fall back to timestamp_ms+pid). The file shape is deliberately simple so
//! shell scripts can grep + `tail -f` without a custom client.

use anyhow::{anyhow, Context, Result};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const AGENTS_FILE_CANDIDATES: &[&str] = &["AGENTS.md", ".crabcc/AGENTS.md"];

/// Default model when the caller doesn't pass `--model`. Opus 4.7 is the
/// strongest agentic model in the Claude 4.x family at the time `crabcc
/// agent` shipped; bump this in lockstep with the project README's
/// recommended model. `claude` (the CLI) ignores `--model` it doesn't
/// recognize and falls through to the user's configured default, so a
/// stale id here degrades gracefully.
const DEFAULT_MODEL: &str = "claude-opus-4-7";
/// Default model when [`Backend::Ollama`] is selected and `--model` is
/// unset. Picks `qwen2.5-coder` from `install/ollama-stack/litellm.config.yaml`
/// — purpose-built for code, matches crabcc's primary workload (symbol
/// lookup + code edits). Override with `--model ollama/<other>`.
const DEFAULT_OLLAMA_MODEL: &str = "ollama/qwen2.5-coder";

/// Where Claude Code looks for skills. `crabcc install-claude` symlinks
/// `skill/crabcc/SKILL.md` into the directory below; if that file is
/// missing we warn the user (without aborting — agent will still run,
/// just without the crabcc primer auto-loaded).
const SKILL_RELATIVE_PATH: &str = ".claude/skills/crabcc/SKILL.md";

/// Which LLM backend the spawned agent talks to. Default is `Claude`,
/// matching pre-issue-#105 behaviour. `Ollama` routes through the local
/// LiteLLM proxy from the bundled Compose stack — see
/// [`crabcc_core::ollama_stack`] and `install/ollama-stack/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Claude,
    Ollama,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Claude => "claude",
            Backend::Ollama => "ollama",
        }
    }
    pub fn from_str(s: &str) -> Result<Self> {
        match s {
            "claude" => {
                eprintln!(
                    "crabcc agent: warning — `--backend claude` is BETA. The default \
                     is now `ollama` (issue #105 LiteLLM proxy + local stack). Pass \
                     `--backend ollama` to silence this notice; `--backend claude` \
                     keeps working but may move to an opt-in path in a future major."
                );
                Ok(Backend::Claude)
            }
            "ollama" => Ok(Backend::Ollama),
            other => Err(anyhow!(
                "unknown agent backend `{other}`; supported: ollama (default), claude (beta)"
            )),
        }
    }
}

/// Where the agent process actually runs. Orthogonal to [`Backend`] —
/// transport is the WHERE, backend is the WHAT-LLM. Default is
/// `Subprocess` (host-side spawn, current behaviour). `Bullmq` enqueues
/// the run on the crabcc-agents BullMQ queue and tails the per-job
/// Redis Stream back into this run's log file; behind the
/// `agents-bullmq` Cargo feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentTransport {
    Subprocess,
    #[cfg(feature = "agents-bullmq")]
    Bullmq,
}

impl AgentTransport {
    /// String form mirrors `Backend::as_str` — public for telemetry /
    /// log lines / future `--transport` clap arg.
    #[allow(dead_code)] // mirrors Backend::as_str; consumed by future CLI plumbing.
    pub fn as_str(self) -> &'static str {
        match self {
            AgentTransport::Subprocess => "subprocess",
            #[cfg(feature = "agents-bullmq")]
            AgentTransport::Bullmq => "bullmq",
        }
    }
    pub fn from_str(s: &str) -> Result<Self> {
        match s {
            "subprocess" | "host" | "local" => Ok(AgentTransport::Subprocess),
            "bullmq" | "queue" | "agents" => {
                #[cfg(not(feature = "agents-bullmq"))]
                {
                    Err(anyhow!(
                        "transport `bullmq` requires this binary to be built with the \
                         `agents-bullmq` Cargo feature. \
                         Rebuild with `cargo install --path crates/crabcc-cli \
                         --features agents-bullmq`."
                    ))
                }
                #[cfg(feature = "agents-bullmq")]
                {
                    Ok(AgentTransport::Bullmq)
                }
            }
            other => Err(anyhow!(
                "unknown agent transport `{other}`; supported: subprocess (default), bullmq"
            )),
        }
    }
}

pub struct AgentRequest<'a> {
    pub prompt: &'a str,
    pub root: &'a Path,
    pub dry_run: bool,
    pub model: Option<String>,
    pub no_refresh: bool,
    pub backend: Backend,
    pub transport: AgentTransport,
}

pub trait AgentRuntime {
    fn run(&self, request: &AgentRequest<'_>, run: &RunDir) -> Result<i32>;
    fn label(&self) -> &'static str;
}

/// Per-invocation state directory under `~/.crabcc/agents/<id>/`.
///
/// Created up-front so we can print the path in the launch banner —
/// the user can `tail -f ~/.crabcc/agents/<id>/log` from another shell
/// before the agent has produced a single byte of output.
pub struct RunDir {
    pub id: String,
    pub dir: PathBuf,
    pub lock_path: PathBuf,
    pub pid_path: PathBuf,
    pub log_path: PathBuf,
    pub meta_path: PathBuf,
}

impl RunDir {
    /// Create the run dir + the bookkeeping files. Caller is responsible
    /// for cleaning up `lock` via [`RunDir::finalize`] on graceful exit;
    /// a leftover `lock` after a crash is the canonical "this run died"
    /// signal.
    pub fn create(home: &Path) -> Result<Self> {
        let id = generate_id();
        let dir = home.join(".crabcc").join("agents").join(&id);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create agent run dir {}", dir.display()))?;
        let lock_path = dir.join("lock");
        let pid_path = dir.join("pid");
        let log_path = dir.join("log");
        let meta_path = dir.join("meta.json");

        // Create lock + log + pid atomically before the child spawns.
        //   lock  — presence signals "run in flight"; removal = graceful exit;
        //           leftover after crash = the "previous run died" signal.
        //   log   — open early so `tail -f` works from the first byte.
        //   pid   — written with "0\n" now; overwritten with the real PID in
        //           write_pid(). This guarantees pid always exists while the
        //           run-dir exists, even during the brief window between
        //           RunDir::create and child.spawn (important for jobs-worker
        //           correlation via agent_folder).
        File::create(&lock_path)
            .with_context(|| format!("create lock file {}", lock_path.display()))?;
        File::create(&log_path)
            .with_context(|| format!("create log file {}", log_path.display()))?;
        std::fs::write(&pid_path, "0\n")
            .with_context(|| format!("create pid file {}", pid_path.display()))?;

        Ok(RunDir {
            id,
            dir,
            lock_path,
            pid_path,
            log_path,
            meta_path,
        })
    }

    pub fn write_meta(&self, req: &AgentRequest<'_>, runtime_label: &str) -> Result<()> {
        let started = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // Truncate the prompt for the meta record — full prompts can be
        // huge (`--run "$(cat huge.md)"` is a real call shape) and we
        // don't want meta.json to grow unbounded.
        let prompt_preview: String = req.prompt.chars().take(240).collect();
        let meta = serde_json::json!({
            "id":             self.id,
            "started_ts":     started,
            "root":           req.root.display().to_string(),
            "backend":        req.backend.as_str(),
            "model":          req.model,
            "runtime":        runtime_label,
            "prompt_chars":   req.prompt.chars().count(),
            "prompt_preview": prompt_preview,
        });
        let body = sonic_rs::to_string_pretty(&meta)?;
        std::fs::write(&self.meta_path, body)
            .with_context(|| format!("write meta {}", self.meta_path.display()))?;
        Ok(())
    }

    pub fn write_pid(&self, pid: u32) -> Result<()> {
        std::fs::write(&self.pid_path, format!("{pid}\n"))
            .with_context(|| format!("write pid file {}", self.pid_path.display()))?;
        Ok(())
    }

    /// Remove the in-flight `lock` marker. Leftover lock = previous run
    /// crashed without finalizing, which is the signal a future
    /// `crabcc agent list` will use.
    pub fn finalize(&self) {
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

/// 16-char lowercase hex id. Reads 8 bytes from `/dev/urandom` when it
/// can; falls back to `timestamp_ms ^ pid` otherwise. Adequate for
/// single-user developer use — we don't need cryptographic uniqueness.
fn generate_id() -> String {
    let mut bytes = [0u8; 8];
    let ok = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .is_ok();
    if !ok {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let pid = std::process::id() as u64;
        let mix = ts ^ (pid.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        bytes.copy_from_slice(&mix.to_le_bytes());
    }
    let mut out = String::with_capacity(16);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Host-subprocess runtime. Looks up `claude` (or `claude-code`) on
/// PATH, builds an argv with `--print "$prompt"` (one-shot mode) plus
/// any system-prompt file the repo ships, tees stdout+stderr into the
/// run-dir log, and waits on the child.
///
/// Auth pass-through: we explicitly pass `HOME` (and the `XDG_*` vars,
/// when set) into the child so Claude Code finds the user's existing
/// `~/.claude/` config — *no separate login flow*. Other env is
/// inherited via `Command`'s default so things like `PATH`, `SHELL`,
/// `TERM` keep working without a per-var allowlist.
pub struct SubprocessRuntime;

impl AgentRuntime for SubprocessRuntime {
    fn label(&self) -> &'static str {
        "subprocess (host)"
    }

    fn run(&self, req: &AgentRequest<'_>, run: &RunDir) -> Result<i32> {
        let claude = find_claude().ok_or_else(|| {
            anyhow!(
                "`claude` CLI not on PATH; install Claude Code first \
                 (https://claude.ai/code) and re-run `crabcc agent --run …`"
            )
        })?;
        let system_prompt = read_system_prompt(req.root);

        // Compose --append-system-prompt: AGENTS.md body (if present) ⊕
        // the loaded internal-agent profile's prompt (if any). At most
        // one --append-system-prompt arg — Claude Code concatenates
        // a single value rather than accumulating multiple invocations.
        let mut sp_body = system_prompt
            .as_ref()
            .map(|s| s.body.clone())
            .unwrap_or_default();
        ACTIVE_PROFILE.with(|cell| {
            if let Some(profile) = cell.borrow().as_ref() {
                if !sp_body.is_empty() {
                    sp_body.push_str("\n\n---\n\n");
                }
                sp_body.push_str(&profile.system_prompt);
            }
        });

        let mut cmd = Command::new(&claude);
        cmd.arg("--print").arg(req.prompt);
        if !sp_body.is_empty() {
            cmd.arg("--append-system-prompt").arg(&sp_body);
        }
        // Profile env exports: CRABCC_BUILD_PROFILE, RUST_LOG, etc.
        // Applied before the auth-passthrough block below so explicit
        // user vars (HOME, ANTHROPIC_API_KEY) win on collision.
        ACTIVE_PROFILE.with(|cell| {
            if let Some(profile) = cell.borrow().as_ref() {
                for (k, v) in profile.env_iter() {
                    cmd.env(k, v);
                }
            }
        });
        // Always pass `--model`. Defaults branch on backend so each
        // invocation is reproducible — relying on the agent CLI's
        // ambient config means two devs on the same prompt can get
        // different answers depending on whose `claude config` we ask.
        let model = req.model.as_deref().unwrap_or(match req.backend {
            Backend::Claude => DEFAULT_MODEL,
            Backend::Ollama => DEFAULT_OLLAMA_MODEL,
        });
        cmd.arg("--model").arg(model);
        cmd.arg("--no-chrome");
        cmd.current_dir(req.root);

        // Auth pass-through. `Command` already inherits env by default,
        // so HOME is forwarded — but be explicit because it's a contract:
        // a future `SandboxRuntime` will need to bind-mount $HOME/.claude/
        // into the sandbox, and grepping for the env-var names here is
        // how that future code will know which vars to plumb.
        let mut auth_vars: Vec<&str> = vec![
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "ANTHROPIC_API_KEY",
        ];
        // Issue #105 — when the agent is configured to talk to the
        // local LiteLLM proxy, forward OLLAMA_BASE_URL + OLLAMA_API_KEY
        // so the spawned subprocess can route LLM calls through the
        // bundled stack instead of Anthropic. Default backend stays
        // Claude; no env-var change for existing flows.
        if req.backend == Backend::Ollama {
            auth_vars.extend(["OLLAMA_BASE_URL", "OLLAMA_API_KEY"]);
        }
        for var in auth_vars {
            if let Ok(v) = std::env::var(var) {
                cmd.env(var, v);
            }
        }

        // PATH bias: prepend ~/.crabcc/bin (which we ensure exists +
        // contains a `crabcc` symlink) and ~/.cargo/bin / ~/.local/bin
        // so the agent's Bash tool calls find the bins we expect even
        // when the user's shell rc differs from ours. We don't replace
        // PATH — we extend its prefix — so `git`, `gh`, `jq`, `rg`,
        // etc. that the user already had stay reachable.
        if let Ok(home) = std::env::var("HOME") {
            let home_path = std::path::Path::new(&home);
            // Best-effort bin-dir setup. Failures (read-only FS, broken
            // symlink) just degrade us to the parent PATH; the agent
            // still runs, just without our preferred binary order.
            let _ = crabcc_viz::runtime::ensure_bin_dir(home_path);
            cmd.env("PATH", crabcc_viz::runtime::agent_path(home_path));
        }

        if req.dry_run {
            print_dry_run(self.label(), &claude, req, system_prompt.as_ref(), run);
            return Ok(0);
        }

        // Pipe both child streams so we can tee into the log file AND
        // forward to the parent's stdout/stderr. The user sees output
        // in real time; the log is a faithful rerun-able transcript.
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawn {}", claude.display()))?;
        run.write_pid(child.id())?;
        run.write_meta(req, self.label())?;

        // Two background threads: one tee's stdout, the other stderr.
        // Each appends to `log` and forwards to the corresponding
        // parent fd. We open the log file twice (once per thread) so
        // we don't need a Mutex around a single shared writer; each
        // append is line-aligned-enough for a transcript.
        let log_for_out = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&run.log_path)?;
        let log_for_err = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&run.log_path)?;
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let h_out = std::thread::spawn(move || tee(stdout, log_for_out, std::io::stdout()));
        let h_err = std::thread::spawn(move || tee(stderr, log_for_err, std::io::stderr()));

        let status = child.wait().context("wait on agent process")?;
        let _ = h_out.join();
        let _ = h_err.join();
        Ok(status.code().unwrap_or(1))
    }
}

/// Copy `src` into `log` AND `out` simultaneously, byte-for-byte.
/// `log` and `out` are independent file handles; we don't need
/// synchronization because each tee thread writes to its own log
/// handle (POSIX guarantees per-write atomicity for small writes
/// up to PIPE_BUF, which 4 KiB chunks comfortably fit under).
fn tee<R: Read, A: Write, B: Write>(mut src: R, mut log: A, mut out: B) {
    let mut buf = [0u8; 4096];
    loop {
        match src.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let _ = log.write_all(&buf[..n]);
                let _ = log.flush();
                let _ = out.write_all(&buf[..n]);
                let _ = out.flush();
            }
            Err(_) => break,
        }
    }
}

#[cfg(feature = "agent-sandbox")]
pub struct SandboxRuntime;

#[cfg(feature = "agent-sandbox")]
impl AgentRuntime for SandboxRuntime {
    fn label(&self) -> &'static str {
        "microsandbox (v3.0 — stub)"
    }
    fn run(&self, _req: &AgentRequest<'_>, _run: &RunDir) -> Result<i32> {
        anyhow::bail!(
            "sandbox runtime is not yet implemented — see \
             https://github.com/peterlodri-sec/crabcc/issues/62 (v3.0). \
             For v2.5.x, omit `--sandbox` to use the host subprocess runtime."
        )
    }
}

struct SystemPrompt {
    path: PathBuf,
    body: String,
}

fn read_system_prompt(root: &Path) -> Option<SystemPrompt> {
    for name in AGENTS_FILE_CANDIDATES {
        let path = root.join(name);
        if let Ok(body) = std::fs::read_to_string(&path) {
            return Some(SystemPrompt { path, body });
        }
    }
    None
}

fn print_dry_run(
    runtime_label: &str,
    binary: &Path,
    req: &AgentRequest<'_>,
    sp: Option<&SystemPrompt>,
    run: &RunDir,
) {
    println!("crabcc agent — dry run (no agent invoked)");
    println!("  runtime         : {runtime_label}");
    println!("  binary          : {}", binary.display());
    println!("  cwd             : {}", req.root.display());
    println!("  run id          : {}", run.id);
    println!("  run dir         : {}", run.dir.display());
    println!("  log (tail -f)   : {}", run.log_path.display());
    let model_resolved = req.model.as_deref().unwrap_or(match req.backend {
        Backend::Claude => DEFAULT_MODEL,
        Backend::Ollama => DEFAULT_OLLAMA_MODEL,
    });
    let model_origin = if req.model.is_some() {
        "explicit"
    } else {
        "default"
    };
    println!("  model           : {model_resolved} ({model_origin})");
    println!(
        "  system prompt   : {}",
        sp.map(|s| format!("{} ({} bytes)", s.path.display(), s.body.len()))
            .unwrap_or_else(|| "(none — agent default)".to_string())
    );
    let preview: String = req.prompt.chars().take(160).collect();
    let suffix = if req.prompt.chars().count() > 160 {
        "…"
    } else {
        ""
    };
    println!(
        "  prompt          : {} chars — \"{preview}{suffix}\"",
        req.prompt.chars().count()
    );
    let auth_status = if std::env::var_os("HOME").is_some() {
        "$HOME present (Claude Code auth at ~/.claude/ inherited)"
    } else {
        "$HOME unset — agent will fail to authenticate"
    };
    println!("  auth            : {auth_status}");
    println!(
        "  refresh first?  : {}",
        if req.no_refresh { "no" } else { "yes" }
    );
    println!();
    println!("(no spawn — re-run without `--dry-run` to actually invoke the agent)");
}

fn find_claude() -> Option<PathBuf> {
    for name in ["claude", "claude-code"] {
        if let Ok(path_var) = std::env::var("PATH") {
            for dir in std::env::split_paths(&path_var) {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME not set; cannot locate ~/.crabcc/agents/"))
}

// Thread-local profile slot. Set before `run()`, consulted by
// SubprocessRuntime::run when composing --append-system-prompt + env.
// A thread-local is the smallest seam that lets `run_with_profile`
// thread the profile through without changing AgentRequest's shape
// (which test fixtures + the original `pub fn run` API depend on).
std::thread_local! {
    pub(crate) static ACTIVE_PROFILE: std::cell::RefCell<Option<crate::agent_profile::AgentProfile>>
        = const { std::cell::RefCell::new(None) };
}

/// Public entry point that loads an internal agent profile alongside
/// the regular run. The profile's composed system prompt is appended
/// to the existing AGENTS.md preamble; its `[env]` is exported to the
/// spawned agent's child process. See `agent_profile.rs`.
pub fn run_with_profile(
    req: AgentRequest<'_>,
    profile: Option<crate::agent_profile::AgentProfile>,
) -> Result<()> {
    if let Some(p) = profile {
        ACTIVE_PROFILE.with(|cell| {
            *cell.borrow_mut() = Some(p);
        });
    }
    let result = run(req);
    ACTIVE_PROFILE.with(|cell| {
        *cell.borrow_mut() = None;
    });
    result
}

pub fn run(req: AgentRequest<'_>) -> Result<()> {
    let home = home_dir()?;
    let run_dir = RunDir::create(&home)?;
    println!(
        "crabcc agent: id={}  log={}",
        run_dir.id,
        run_dir.log_path.display()
    );

    // Per-model banner (one stderr line). Looks up the .info file for
    // the resolved provider+model and prints it before any heavy work.
    // Best-effort: silent on missing file or read error.
    let provider = match req.backend {
        Backend::Claude => "claude",
        Backend::Ollama => "ollama",
    };
    let resolved_model = req.model.as_deref().unwrap_or(match req.backend {
        Backend::Claude => DEFAULT_MODEL,
        Backend::Ollama => DEFAULT_OLLAMA_MODEL,
    });
    // For provider "ollama" the resolved_model often comes prefixed
    // (e.g. `ollama/qwen2.5-coder`). The .info file is keyed on the
    // model name without the prefix; strip it for the lookup.
    let bare_name = resolved_model
        .strip_prefix("ollama/")
        .unwrap_or(resolved_model);
    if let Ok(Some(info)) = crate::model_info::read(&home, provider, bare_name) {
        eprintln!("crabcc agent: {}", crate::model_info::banner_line(&info));
    }

    // Open the singleton runs DB. Best-effort: if it fails (locked,
    // disk full), the agent still runs — the menubar's pgrep + lockfile
    // fallback will catch this run.
    let db_path = crate::agent_runs_db::default_db_path(&home);
    let db_conn = crate::agent_runs_db::open(&db_path).ok();
    if let Some(c) = &db_conn {
        let _ = crate::agent_runs_db::reap_stale(c);
        let _ = crate::agent_runs_db::insert_run(
            c,
            &run_dir.id,
            req.root,
            "subprocess",
            req.model.as_deref(),
            &run_dir.log_path,
            &run_dir.meta_path,
        );
        let _ = crate::agent_runs_db::update_pid(c, &run_dir.id, std::process::id());
    }

    // Skill check is cheap (one stat); print a hint when missing so a
    // fresh dev who hasn't run `crabcc install-claude` yet gets a
    // breadcrumb instead of a silently-uninformed agent. Don't auto-
    // install — the install path needs the source repo, which the
    // user may not have checked out at a stable location.
    let skill = home.join(SKILL_RELATIVE_PATH);
    if !skill.exists() && !req.dry_run {
        eprintln!(
            "crabcc agent: warning — skill not found at {}; \
             run `crabcc install-claude` once to load the crabcc primer \
             into Claude Code (the agent will still work, just without \
             the auto-loaded skill)",
            skill.display()
        );
    }

    // Make sure the agent lands in a fully-initialized repo: index built,
    // graph sidecar present, memory db open. We reuse `go::init` so the
    // initialization contract is identical between `crabcc go` and
    // `crabcc agent` — a future bump to either picks the other up.
    if !req.dry_run {
        let db = req.root.join(".crabcc").join("index.db");
        if req.no_refresh {
            // Honour --no-refresh literally: only open existing state,
            // don't run a full_index even if .crabcc is missing. Agents
            // that wrap `crabcc agent` in a script may have already
            // brought the index up to date and want no extra work.
            tracing::debug!("crabcc agent: --no-refresh set; skipping ensure-initialized");
        } else {
            match crate::go::init(req.root, &db) {
                Ok(report) => {
                    eprintln!(
                        "crabcc agent: index ready ({} files, {} symbols, {} graph edges)",
                        report.files_indexed, report.symbols, report.graph_edges
                    );
                }
                Err(e) => {
                    // Non-fatal — agent can still run without an index,
                    // crabcc MCP will just return empty results. Surface
                    // the error so the user knows why the agent's tool
                    // calls aren't finding anything.
                    eprintln!("crabcc agent: warning — init failed: {e:#}");
                }
            }
        }
    }

    // Issue #105 — when the caller picked the Ollama backend, make
    // sure the local stack is up before we spawn the agent. Failures
    // surface on the agent's stderr through anyhow's chain. Skipped
    // on --dry-run so wiring tests stay docker-free.
    if req.backend == Backend::Ollama && !req.dry_run {
        crabcc_core::ollama_stack::check_docker().with_context(|| {
            "agent --backend ollama needs Docker; install from \
             https://docs.docker.com/get-docker/ then `crabcc install-claude \
             --with-ollama-stack`. Run `crabcc doctor docker` for diagnostics."
        })?;
        let opts = crabcc_core::ollama_stack::Options::new()
            .with_correlation_id(format!("agent-{}", run_dir.id));
        let up = crabcc_core::ollama_stack::ensure_up(&opts).with_context(|| {
            "agent --backend ollama: ensure_up() failed. \
             Run `crabcc doctor stack` to inspect container health."
        })?;
        eprintln!(
            "crabcc agent: ollama stack ready ({} services, {} ms)",
            up.services_healthy.len(),
            up.duration_ms
        );
    }

    let result = match req.transport {
        AgentTransport::Subprocess => SubprocessRuntime.run(&req, &run_dir),
        #[cfg(feature = "agents-bullmq")]
        AgentTransport::Bullmq => crate::agent_bullmq::BullmqRuntime.run(&req, &run_dir),
    };
    run_dir.finalize();

    // Record completion in the singleton runs DB. Use -1 as the exit
    // code for "runtime errored before claude returned" so the row
    // doesn't sit in 'running' forever.
    if let Some(c) = &db_conn {
        let exit_code = match &result {
            Ok(code) => *code,
            Err(_) => -1,
        };
        let _ = crate::agent_runs_db::mark_finished(c, &run_dir.id, exit_code);
    }

    let code = result?;
    if code != 0 {
        anyhow::bail!(
            "agent exited with status {code} (logs: {})",
            run_dir.log_path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn backend_round_trip_strings() {
        assert_eq!(Backend::from_str("claude").unwrap(), Backend::Claude);
        assert_eq!(Backend::from_str("ollama").unwrap(), Backend::Ollama);
        assert_eq!(Backend::Claude.as_str(), "claude");
        assert_eq!(Backend::Ollama.as_str(), "ollama");
    }

    #[test]
    fn backend_rejects_unknown() {
        let err = Backend::from_str("anthropic-direct").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown agent backend"));
        assert!(msg.contains("supported"));
    }

    #[test]
    fn rundir_create_makes_lock_and_log() {
        let home = tempdir().unwrap();
        let run = RunDir::create(home.path()).unwrap();
        assert!(run.dir.starts_with(home.path()));
        assert!(run.dir.exists(), "run dir must exist");
        assert!(run.lock_path.exists(), "lock must be present at start");
        assert!(
            run.log_path.exists(),
            "log must be touched up-front for tail -f"
        );
        // ID shape: 16 lowercase hex chars.
        assert_eq!(run.id.len(), 16, "id should be 16 hex chars: {}", run.id);
        assert!(
            run.id.chars().all(|c| c.is_ascii_hexdigit()),
            "id should be hex: {}",
            run.id
        );
    }

    #[test]
    fn rundir_finalize_removes_lock() {
        let home = tempdir().unwrap();
        let run = RunDir::create(home.path()).unwrap();
        assert!(run.lock_path.exists());
        run.finalize();
        assert!(!run.lock_path.exists(), "finalize must remove lock");
    }

    #[test]
    fn rundir_write_pid_persists_pid_file() {
        let home = tempdir().unwrap();
        let run = RunDir::create(home.path()).unwrap();
        run.write_pid(12345).unwrap();
        let body = std::fs::read_to_string(&run.pid_path).unwrap();
        assert_eq!(body.trim(), "12345");
    }

    #[test]
    fn rundir_write_meta_includes_prompt_preview_and_runtime() {
        let home = tempdir().unwrap();
        let root = tempdir().unwrap();
        let run = RunDir::create(home.path()).unwrap();
        let req = AgentRequest {
            prompt: "tell me about Store::open",
            root: root.path(),
            dry_run: true,
            model: Some("claude-sonnet-4-6".into()),
            no_refresh: true,
            backend: Backend::Claude,
            transport: AgentTransport::Subprocess,
        };
        run.write_meta(&req, "subprocess (host)").unwrap();
        let body = std::fs::read_to_string(&run.meta_path).unwrap();
        assert!(body.contains("Store::open"), "meta missing prompt: {body}");
        assert!(body.contains("subprocess"), "meta missing runtime: {body}");
        assert!(
            body.contains("claude-sonnet-4-6"),
            "meta missing model: {body}"
        );
    }

    #[test]
    fn generate_id_is_unique_across_calls() {
        // Even at /dev/urandom failures, the timestamp+pid fallback
        // varies across calls. We guard the test by sleeping for a
        // submillisecond's worth of wall-clock between calls — enough
        // for nanos to advance even on a fast loop.
        let a = generate_id();
        std::thread::sleep(std::time::Duration::from_micros(50));
        let b = generate_id();
        assert_ne!(a, b, "two consecutive ids should differ ({a} == {b})");
        assert_eq!(a.len(), 16);
        assert_eq!(b.len(), 16);
    }

    #[test]
    fn read_system_prompt_finds_agents_md_at_root() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "be terse\n").unwrap();
        let sp = read_system_prompt(dir.path()).expect("AGENTS.md should be picked up");
        assert!(sp.path.ends_with("AGENTS.md"));
        assert_eq!(sp.body, "be terse\n");
    }

    #[test]
    fn read_system_prompt_falls_back_to_crabcc_dir() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".crabcc")).unwrap();
        std::fs::write(
            dir.path().join(".crabcc").join("AGENTS.md"),
            "scoped to .crabcc\n",
        )
        .unwrap();
        let sp = read_system_prompt(dir.path()).expect(".crabcc/AGENTS.md should be picked up");
        assert!(sp.path.to_string_lossy().contains(".crabcc"));
    }

    #[test]
    fn read_system_prompt_prefers_root_over_crabcc_dir() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".crabcc")).unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "root wins\n").unwrap();
        std::fs::write(
            dir.path().join(".crabcc").join("AGENTS.md"),
            "scoped loses\n",
        )
        .unwrap();
        let sp = read_system_prompt(dir.path()).unwrap();
        assert_eq!(sp.body, "root wins\n");
    }

    #[test]
    fn read_system_prompt_returns_none_when_absent() {
        let dir = tempdir().unwrap();
        assert!(read_system_prompt(dir.path()).is_none());
    }

    #[test]
    fn tee_copies_to_both_destinations() {
        let mut log = Vec::<u8>::new();
        let mut out = Vec::<u8>::new();
        tee(&b"hello, world\n"[..], &mut log, &mut out);
        assert_eq!(log, b"hello, world\n");
        assert_eq!(out, b"hello, world\n");
    }
}
