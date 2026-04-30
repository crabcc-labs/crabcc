//! `ccc` — high-level combo CLI (issue #74).
//!
//! Sibling binary to `crabcc`. Same crate, same release profile (LTO=fat,
//! strip=true), same install path. Difference: `ccc` exposes ~6 combo
//! verbs (find / list / index / memory / info / setup) instead of
//! `crabcc`'s 24+ granular ones. The granular CLI stays — `crabcc` is
//! the low-level surface; `ccc` is the user-friendly one.
//!
//! Implementation: thin subprocess wrapper. `ccc` parses combo args,
//! translates them to the equivalent `crabcc` invocation, and execs.
//! No business logic lives here — keeps `crabcc` as the single source
//! of truth and `ccc` reduces to ~250 lines of arg-mapping.

use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use std::process::{exit, Command};

#[derive(Parser, Debug)]
#[command(
    name = "ccc",
    version,
    about = "High-level combo CLI for crabcc — find / list / index / memory / info / setup.",
    long_about = "\
ccc — the user-friendly surface for `crabcc`.

Six combo verbs (find / list / index / memory / info / setup) translate to the
right granular `crabcc` invocation under the hood. ccc is a thin subprocess
wrapper, ~25KB stripped vs `crabcc`'s ~45MB; both share the same install dir.

Underlying tool:
  crabcc — symbol index for AI coding agents (24+ granular subcommands)
           Repo:  https://github.com/peterlodri-sec/crabcc
           Run:   `crabcc --help` for the low-level surface

Locating crabcc:
  1. $CRABCC_BIN if set
  2. Sibling of this exe (paired install)
  3. PATH lookup

Examples:
  ccc find Foo                     → crabcc sym Foo
  ccc find Foo --mode references   → crabcc refs Foo
  ccc find Foo --mode callers      → crabcc callers Foo
  ccc list --files --ext rs        → crabcc files --ext rs
  ccc index --delta                → crabcc refresh --delta
  ccc memory search 'auth flow'    → crabcc memory search 'auth flow'
  ccc info --tokens                → crabcc track
  ccc setup --claude               → crabcc install-claude
  ccc setup --ollama-up            → crabcc ollama-stack up      (issue #105)
  ccc setup --ollama-status        → crabcc ollama-stack status

Issue #74 — v3.0 CLI consolidation. The granular `crabcc` subcommands stay
available unchanged; running them prints a one-line stderr hint pointing at
the ccc equivalent (suppress with CRABCC_NO_HINT=1)."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Find a symbol/reference/caller. Combo of crabcc sym / refs / callers / fuzzy / prefix / grep.
    Find {
        name: String,
        #[arg(long, value_enum, default_value_t = FindMode::Definition)]
        mode: FindMode,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        out: Option<String>,
        #[arg(long, conflicts_with = "out")]
        files_only: bool,
        #[arg(long, conflicts_with = "out", conflicts_with = "files_only")]
        count_only: bool,
    },
    /// List indexed entities. Combo of crabcc files / graph cycles / graph orphans.
    List {
        #[arg(long, group = "list_what")]
        files: bool,
        #[arg(long, group = "list_what")]
        cycles: bool,
        #[arg(long, group = "list_what")]
        orphans: bool,
        #[arg(long)]
        under: Option<String>,
        #[arg(long)]
        ext: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Index. Combo of crabcc index / refresh / watch / compress / refresh-tantivy.
    Index {
        #[arg(long, group = "index_mode")]
        delta: bool,
        #[arg(long, group = "index_mode")]
        watch: bool,
        #[arg(long, group = "index_mode")]
        compress: bool,
        #[arg(long, group = "index_mode")]
        rebuild_fts: bool,
        /// Stream tracing logs (info-level, scoped to crabcc) to stderr
        /// alongside the JSON stats. Equivalent to setting
        /// `RUST_LOG=crabcc=info,crabcc_core=info,crabcc_cli=info` for
        /// the underlying `crabcc index` invocation.
        #[arg(long)]
        logs: bool,
    },
    /// Memory operations. Pass-through to `crabcc memory ...`.
    Memory {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Info. Combo of crabcc info / track / upgrade --check.
    Info {
        #[arg(long, group = "info_what")]
        tokens: bool,
        #[arg(long, group = "info_what")]
        upgrade_check: bool,
        #[arg(long, group = "info_what")]
        status_line: bool,
    },
    /// Setup. Combo of install-claude / completions / upgrade /
    /// ollama-stack ops. Ollama-stack flags are intentionally folded
    /// into setup (not exposed as a top-level `ccc ollama-stack`) per
    /// issue #105 — operator surface lives under the setup/install
    /// umbrella.
    Setup {
        #[arg(long, group = "setup_what")]
        claude: bool,
        #[arg(long, group = "setup_what", value_name = "SHELL")]
        completions: Option<String>,
        #[arg(long, group = "setup_what")]
        upgrade: bool,
        /// Bring the Ollama auth stack up — `crabcc ollama-stack up`.
        /// Requires Docker; see install/ollama-stack/README.md.
        #[arg(long, group = "setup_what")]
        ollama_up: bool,
        /// Stop the Ollama auth stack — `crabcc ollama-stack down`.
        /// Volumes (model cache) are kept; pass `--ollama-down-volumes`
        /// to wipe them.
        #[arg(long, group = "setup_what")]
        ollama_down: bool,
        /// Stop the Ollama stack and wipe model-cache + caddy volumes.
        /// Implies --ollama-down.
        #[arg(long, group = "setup_what")]
        ollama_down_volumes: bool,
        /// Print Ollama stack status (JSON) — `crabcc ollama-stack status`.
        #[arg(long, group = "setup_what")]
        ollama_status: bool,
        /// Refresh upstream images — `crabcc ollama-stack pull`.
        /// Combine with --ollama-up afterward to recreate services.
        #[arg(long, group = "setup_what")]
        ollama_pull: bool,
    },
}

#[derive(Debug, Copy, Clone, ValueEnum)]
enum FindMode {
    Definition,
    References,
    Callers,
    Fuzzy,
    Prefix,
    Grep,
}

impl FindMode {
    fn subcmd(self) -> &'static str {
        match self {
            FindMode::Definition => "sym",
            FindMode::References => "refs",
            FindMode::Callers => "callers",
            FindMode::Fuzzy => "fuzzy",
            FindMode::Prefix => "prefix",
            FindMode::Grep => "grep",
        }
    }
}

fn locate_crabcc() -> PathBuf {
    if let Ok(p) = std::env::var("CRABCC_BIN") {
        return PathBuf::from(p);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let candidate = parent.join("crabcc");
            if candidate.exists() {
                return candidate;
            }
        }
    }
    PathBuf::from("crabcc")
}

/// Turn on info-level tracing for the next subprocess `run()` call.
/// Used by `--logs` flags: we set `RUST_LOG` in our own env before the
/// subprocess inherits it. Scoped to `crabcc*` so tantivy chatter stays
/// suppressed.
fn set_log_filter() {
    if std::env::var_os("RUST_LOG").is_none() {
        std::env::set_var(
            "RUST_LOG",
            "crabcc=info,crabcc_core=info,crabcc_cli=info,crabcc_mcp=info,crabcc_memory=info",
        );
    }
}

fn run(args: &[&str]) -> ! {
    let crabcc = locate_crabcc();
    let status = Command::new(&crabcc)
        .args(args)
        .env("CCC_NO_WARN", "1")
        .status();
    match status {
        Ok(s) => exit(s.code().unwrap_or(1)),
        Err(e) => {
            eprintln!(
                "ccc: failed to exec {} ({e}). Set CRABCC_BIN, or co-install crabcc + ccc.",
                crabcc.display()
            );
            exit(2);
        }
    }
}

fn main() {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Find {
            name,
            mode,
            limit,
            out,
            files_only,
            count_only,
        } => {
            let mut args: Vec<String> = vec![mode.subcmd().to_string(), name];
            if let Some(l) = limit {
                args.push("--limit".into());
                args.push(l.to_string());
            }
            let mode_arg = if let Some(m) = out {
                Some(m)
            } else if files_only {
                Some("files".into())
            } else if count_only {
                Some("count".into())
            } else {
                None
            };
            if let Some(m) = mode_arg {
                if matches!(mode, FindMode::References | FindMode::Callers) {
                    args.push("--mode".into());
                    args.push(m);
                }
            }
            let r: Vec<&str> = args.iter().map(String::as_str).collect();
            run(&r);
        }
        Cmd::List {
            files,
            cycles,
            orphans,
            under,
            ext,
            limit,
        } => {
            if cycles {
                run(&["graph", "cycles"]);
            }
            if orphans {
                run(&["graph", "orphans"]);
            }
            let _ = files;
            let mut args: Vec<String> = vec!["files".into()];
            if let Some(u) = under {
                args.push("--under".into());
                args.push(u);
            }
            if let Some(e) = ext {
                args.push("--ext".into());
                args.push(e);
            }
            if let Some(l) = limit {
                args.push("--limit".into());
                args.push(l.to_string());
            }
            let r: Vec<&str> = args.iter().map(String::as_str).collect();
            run(&r);
        }
        Cmd::Index {
            delta,
            watch,
            compress,
            rebuild_fts,
            logs,
        } => {
            // `--logs` is a flag-of-flags: it doesn't pick a different
            // sub-mode, it just turns on RUST_LOG for whatever sub-mode
            // the caller already chose. The plain `index` (default) is
            // the most useful pairing in practice.
            if logs {
                set_log_filter();
            }
            if watch {
                run(&["watch"]);
            }
            if compress {
                run(&["compress", "--rebuild"]);
            }
            if rebuild_fts {
                run(&["refresh-tantivy"]);
            }
            if delta {
                run(&["refresh", "--delta"]);
            }
            run(&["index"]);
        }
        Cmd::Memory { args } => {
            let mut full: Vec<&str> = vec!["memory"];
            full.extend(args.iter().map(String::as_str));
            run(&full);
        }
        Cmd::Info {
            tokens,
            upgrade_check,
            status_line,
        } => {
            if tokens {
                run(&["track"]);
            }
            if upgrade_check {
                run(&["upgrade", "--check"]);
            }
            if status_line {
                run(&["info", "--status-line"]);
            }
            run(&["info"]);
        }
        Cmd::Setup {
            claude,
            completions,
            upgrade,
            ollama_up,
            ollama_down,
            ollama_down_volumes,
            ollama_status,
            ollama_pull,
        } => {
            if claude {
                run(&["install-claude"]);
            }
            if let Some(shell) = completions {
                run(&["completions", &shell]);
            }
            if upgrade {
                run(&["upgrade"]);
            }
            if ollama_up {
                run(&["ollama-stack", "up"]);
            }
            if ollama_down_volumes {
                run(&["ollama-stack", "down", "--volumes"]);
            }
            if ollama_down {
                run(&["ollama-stack", "down"]);
            }
            if ollama_status {
                run(&["ollama-stack", "status"]);
            }
            if ollama_pull {
                run(&["ollama-stack", "pull"]);
            }
            eprintln!(
                "ccc setup: pass --claude / --completions <SHELL> / --upgrade / \
                 --ollama-up / --ollama-down[-volumes] / --ollama-status / --ollama-pull.\n\
                 See `ccc setup --help`."
            );
            exit(2);
        }
    }
}
