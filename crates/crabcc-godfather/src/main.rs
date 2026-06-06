//! `crabcc-godfather` — standalone supervisor binary.
//!
//! Subcommands:
//!
//!   * `daemon` — long-running process: registers itself in
//!     `_crab_session(app=godfather)`, emits a heartbeat every 30 s,
//!     runs the cleanup pruner once per `Retention::prune_interval_secs`.
//!     Exits cleanly on SIGINT.
//!   * `watch --pid X --app NAME` — supervise a foreign process.
//!     Records resource samples + emits a crash event on exit.
//!   * `kill --session ID [--force]` — SIGTERM (or SIGKILL with `--force`)
//!     the recorded PID.
//!   * `restart --app NAME` — kill + relaunch a known launch shape.
//!   * `attach --session ID` — print `lldb -p <pid>` for the
//!     session's PID.
//!   * `status` / `status --json` — a one-page rollup the dashboard
//!     reads via `gh-style stdout` parsing.
//!   * `dump --limit N [--severity X]` — recent events.
//!   * `prune` — force a cleanup run regardless of last-pruned-at.
//!   * `report --crash ID` — emit the markdown crash report to stdout.
//!   * `gh-issue --crash ID --repo OWNER/REPO` — file a GitHub issue.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use crabcc_godfather::{
    cleanup::Retention,
    control::{self, KillSignal},
    event::Severity,
    godfather::{Godfather, InstallSource},
    report,
    watch::{WatchConfig, WatchHandle},
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "crabcc-godfather",
    version,
    about = "Crabcc supervisor — sessions, events, crash reports, process control"
)]
struct Cli {
    /// Override the DB path (default: `~/.crabcc/_internal.db` or
    /// `$CRABCC_HOME/_internal.db`).
    #[arg(long, global = true)]
    db: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Long-running supervisor — heartbeats + cleanup. Exits on SIGINT.
    Daemon {
        /// Heartbeat / sample interval. Default 30 s.
        #[arg(long, default_value_t = 30)]
        interval_secs: u64,
    },
    /// Watch a foreign process by PID.
    Watch {
        #[arg(long)]
        pid: u32,
        #[arg(long)]
        app: String,
        /// Optional log file to tail on exit (writes the last 4 KiB
        /// into the crash row's `log_tail`).
        #[arg(long)]
        log: Option<PathBuf>,
    },
    /// Send SIGTERM (or SIGKILL with `--force`) to the session's PID.
    Kill {
        #[arg(long)]
        session: String,
        #[arg(long)]
        force: bool,
    },
    /// Relaunch a known app (`viz`).
    Restart {
        #[arg(long)]
        app: String,
    },
    /// Print `lldb -p <pid>` for the session's PID.
    Attach {
        #[arg(long)]
        session: String,
    },
    /// One-page rollup. `--json` for machine-readable output.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Recent events.
    Dump {
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long, value_enum)]
        severity: Option<SeverityArg>,
    },
    /// Force a cleanup pass.
    Prune,
    /// Emit the markdown crash report to stdout.
    Report {
        #[arg(long)]
        crash: i64,
    },
    /// File a GitHub issue with the crash report.
    GhIssue {
        #[arg(long)]
        crash: i64,
        #[arg(long, default_value = "peterlodri-sec/crabcc")]
        repo: String,
    },
    /// One-shot install fingerprint (for installer scripts).
    RecordInstall {
        #[arg(long)]
        version: String,
        #[arg(long, value_enum)]
        source: InstallSourceArg,
    },
}

#[derive(Copy, Clone, ValueEnum)]
enum SeverityArg {
    Debug,
    Info,
    Warn,
    Error,
    Crash,
}
impl From<SeverityArg> for Severity {
    fn from(s: SeverityArg) -> Self {
        match s {
            SeverityArg::Debug => Self::Debug,
            SeverityArg::Info => Self::Info,
            SeverityArg::Warn => Self::Warn,
            SeverityArg::Error => Self::Error,
            SeverityArg::Crash => Self::Crash,
        }
    }
}

#[derive(Copy, Clone, ValueEnum)]
enum InstallSourceArg {
    Cargo,
    GithubRelease,
    Homebrew,
    Source,
    Other,
}
impl From<InstallSourceArg> for InstallSource {
    fn from(s: InstallSourceArg) -> Self {
        match s {
            InstallSourceArg::Cargo => Self::Cargo,
            InstallSourceArg::GithubRelease => Self::GithubRelease,
            InstallSourceArg::Homebrew => Self::Homebrew,
            InstallSourceArg::Source => Self::Source,
            InstallSourceArg::Other => Self::Other,
        }
    }
}

fn open_g(db: Option<&PathBuf>) -> Result<Godfather> {
    if let Some(p) = db {
        Godfather::open_at(p)
    } else {
        Godfather::open()
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let g = open_g(cli.db.as_ref())?;
    g.record_host_info()?;

    match cli.cmd {
        Cmd::Daemon { interval_secs } => run_daemon(g, interval_secs),
        Cmd::Watch { pid, app, log } => run_watch(g, pid, app, log),
        Cmd::Kill { session, force } => run_kill(&g, &session, force),
        Cmd::Restart { app } => run_restart(&g, &app),
        Cmd::Attach { session } => run_attach(&g, &session),
        Cmd::Status { json } => run_status(&g, json),
        Cmd::Dump { limit, severity } => run_dump(&g, limit, severity.map(Into::into)),
        Cmd::Prune => run_prune(&g),
        Cmd::Report { crash } => run_report(&g, crash),
        Cmd::GhIssue { crash, repo } => run_gh_issue(&g, crash, &repo),
        Cmd::RecordInstall { version, source } => run_record_install(&g, &version, source.into()),
    }
}

fn run_daemon(godfather: Godfather, interval_secs: u64) -> Result<()> {
    let pid = std::process::id();
    let session_id = godfather.record_session_start("godfather", env!("CARGO_PKG_VERSION"), pid)?;
    let _ = godfather.record_event(
        Some(&session_id),
        Severity::Info,
        "godfather",
        "lifecycle",
        "daemon started",
        Some(&serde_json::json!({"pid": pid})),
    );

    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    ctrlc::set_handler(move || s2.store(true, Ordering::SeqCst)).ok();

    let interval = Duration::from_secs(interval_secs.max(1));
    while !stop.load(Ordering::SeqCst) {
        let _ = godfather.record_event(
            Some(&session_id),
            Severity::Debug,
            "godfather",
            "heartbeat",
            "daemon tick",
            None,
        );
        let _ = crabcc_godfather::cleanup::prune_if_due(godfather.conn(), &Retention::default());
        std::thread::sleep(interval);
    }

    godfather.record_session_end(&session_id, Some(0), None)?;
    let _ = godfather.record_event(
        Some(&session_id),
        Severity::Info,
        "godfather",
        "lifecycle",
        "daemon stopped",
        None,
    );
    Ok(())
}

fn run_watch(godfather: Godfather, pid: u32, app: String, log: Option<PathBuf>) -> Result<()> {
    // Record a session for the watched app — caller is the
    // supervisor binary, but the row attributes to the watched app
    // so the resource-sample chart on the dashboard shows it under
    // the right name.
    let session_id = godfather.record_session_start(&app, "watched", pid)?;
    let mut config = WatchConfig::new(app.clone(), pid, session_id.clone());
    config.log_path = log;
    let handle = WatchHandle::spawn(godfather, config)?;
    println!("watching {app} (pid {pid}) — Ctrl-C to stop");
    handle.join();
    Ok(())
}

fn run_kill(godfather: &Godfather, session: &str, force: bool) -> Result<()> {
    let signal = if force {
        KillSignal::Kill
    } else {
        KillSignal::Term
    };
    control::kill_session(godfather, session, signal)?;
    println!("ok");
    Ok(())
}

fn run_restart(godfather: &Godfather, app: &str) -> Result<()> {
    let new_pid = control::restart_app(godfather, app)?;
    println!("relaunched {app} as pid {new_pid}");
    Ok(())
}

fn run_attach(godfather: &Godfather, session: &str) -> Result<()> {
    let cmd = control::attach_command(godfather, session)?;
    println!("{cmd}");
    Ok(())
}

fn run_status(godfather: &Godfather, json: bool) -> Result<()> {
    let host = godfather.host_info()?;
    let active = godfather.list_active_sessions(20)?;
    let install_time = godfather.metadata("install_time")?;
    let install_version = godfather.metadata("install_version")?;
    let install_source = godfather.metadata("install_source")?;
    let recent_crashes = godfather.list_recent_events(5, Some(Severity::Crash))?;

    if json {
        let payload = serde_json::json!({
            "telemetry_enabled": godfather.telemetry_enabled(),
            "install": {
                "time": install_time,
                "version": install_version,
                "source": install_source,
            },
            "host": host,
            "active_sessions": active,
            "recent_crashes": recent_crashes,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        println!("crabcc-godfather");
        println!(
            "  telemetry: {}",
            if godfather.telemetry_enabled() {
                "on"
            } else {
                "off"
            }
        );
        println!(
            "  install:   v{} ({})",
            install_version.as_deref().unwrap_or("?"),
            install_source.as_deref().unwrap_or("?")
        );
        if let Some(h) = host {
            println!(
                "  host:      {} {} ({}, {} cores, {} MB)",
                h.os, h.os_version, h.arch, h.cpu_count, h.total_memory_mb
            );
        }
        println!("  active:    {} session(s)", active.len());
        for s in &active {
            println!(
                "    - {} `{}` v{} pid {}",
                s.app,
                &s.id[..8.min(s.id.len())],
                s.version,
                s.pid
            );
        }
        println!("  crashes:   {} recent", recent_crashes.len());
    }
    Ok(())
}

fn run_dump(godfather: &Godfather, limit: usize, sev: Option<Severity>) -> Result<()> {
    let evs = godfather.list_recent_events(limit, sev)?;
    for e in evs {
        println!(
            "{:>10} {:>5} {:>10}/{:<12} {}",
            e.ts,
            e.severity.as_str(),
            e.source,
            e.category,
            e.message
        );
    }
    Ok(())
}

fn run_prune(godfather: &Godfather) -> Result<()> {
    let stats = crabcc_godfather::cleanup::prune_now(godfather.conn(), &Retention::default())?;
    println!(
        "pruned: events={} samples={} sessions={} crashes={} vacuumed={}",
        stats.events_deleted,
        stats.samples_deleted,
        stats.sessions_deleted,
        stats.crashes_deleted,
        stats.vacuumed
    );
    Ok(())
}

fn run_report(godfather: &Godfather, crash: i64) -> Result<()> {
    let md = report::build_report(godfather, crash)?;
    println!("{md}");
    Ok(())
}

fn run_gh_issue(godfather: &Godfather, crash: i64, repo: &str) -> Result<()> {
    let url = report::open_gh_issue(godfather, crash, repo)
        .with_context(|| format!("open gh issue for crash {crash}"))?;
    println!("{url}");
    Ok(())
}

fn run_record_install(godfather: &Godfather, version: &str, source: InstallSource) -> Result<()> {
    godfather.record_install_once(version, source)?;
    println!("recorded install v{version} ({})", source.as_str());
    Ok(())
}
