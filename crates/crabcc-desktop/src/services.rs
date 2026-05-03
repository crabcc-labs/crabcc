//! Backend stack bootstrap.
//!
//! Called by `main` before the GPUI loop opens the window. Ensures
//! the docker-compose stack at `install/dev/docker-compose.yml` is
//! up and `crabcc serve` is responding on `127.0.0.1:7878`. The
//! backend's reachability is what makes the dashboard useful — if
//! the user fires up the desktop binary on a fresh machine, we'd
//! rather start the stack for them than greet them with an empty
//! screen full of red toasts.
//!
//! ## Order of operations
//!
//! 1. Honour `CRABCC_DESKTOP_SKIP_SERVICES` — devs who manage their
//!    own stack can opt out entirely.
//! 2. Fast-path probe: if `/api/health` already answers (someone is
//!    running `crabcc serve` natively, or the stack is already up),
//!    return early and don't touch docker.
//! 3. Otherwise: invoke `docker compose -f .../docker-compose.yml
//!    up -d`. Idempotent — succeeds whether the containers already
//!    exist or not.
//! 4. Poll `/api/health` until ready or the deadline lapses.
//!
//! ## Failure mode
//!
//! All errors here are non-fatal — the GUI proceeds either way.
//! Backend reachability surfaces through the existing prefetch
//! Danger toast (track C.0 slice 3); this module's job is just to
//! reduce the chance the operator sees them at all.

use std::process::Command;
use std::time::{Duration, Instant};

use crate::api::Client;

/// Compose file path, computed at build time from the crate's
/// manifest dir. The desktop crate is workspace-excluded (see
/// README "Why standalone"), so `CARGO_MANIFEST_DIR` lands at
/// `<repo>/crates/crabcc-desktop` reliably.
///
/// Future packaging work (.app bundle): embed the compose file
/// in the bundle's `Resources/` and resolve the path at runtime
/// instead. For `cargo run` from anywhere this constant is
/// already correct.
const COMPOSE_FILE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../install/dev/docker-compose.yml"
);

/// Max time to wait for `crabcc serve` to start answering after
/// `docker compose up`. Tuned for cold cache + first-time image
/// pull on a typical laptop. The container itself comes up in
/// ~1s once the image exists; the binary's `serve` command needs
/// a couple of seconds to bind. 30s is generous headroom.
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Sleep between health-probe attempts. The `Client`'s built-in
/// 5s per-request timeout is the dominant cost; this just keeps
/// us off the CPU between cycles.
const HEALTH_PROBE_SLEEP: Duration = Duration::from_millis(500);

/// Env var that skips the entire bootstrap. Devs who run
/// `crabcc serve` from a separate terminal — or run the whole
/// stack from a different compose file — set this to keep us
/// out of their way.
const SKIP_ENV: &str = "CRABCC_DESKTOP_SKIP_SERVICES";

/// Env var for the symmetric counterpart — when set, the binary's
/// SIGINT handler runs `docker compose down` before exiting. Off
/// by default because most users have multiple consumers of the
/// stack (web dashboard, agents, telegram bot) and tearing it down
/// on every desktop quit would be surprising.
pub const STOP_ON_EXIT_ENV: &str = "CRABCC_DESKTOP_STOP_SERVICES_ON_EXIT";

/// Outcome of a bootstrap attempt — surfaced to the caller for
/// logging and (later slice) toasting.
#[derive(Debug)]
pub enum BootstrapOutcome {
    /// Skipped because the env var was set.
    SkippedByEnv,
    /// Backend was already reachable — no docker action taken.
    AlreadyRunning,
    /// We ran `docker compose up` and the backend came up within
    /// the deadline.
    StartedViaCompose,
    /// `docker compose up` ran (either successfully or not) but
    /// the backend did not become reachable within the deadline.
    /// `last_error` is the most recent probe error.
    StartedButNotReady { last_error: String },
    /// Docker daemon wasn't reachable. Nothing was done.
    DockerUnavailable,
    /// `docker compose up` returned a non-zero exit. `stderr` has
    /// the captured output for diagnostic logging.
    ComposeFailed { stderr: String },
}

/// Run the bootstrap. Designed to be called once from `main` before
/// the GPUI window opens. Synchronous — blocks until the backend
/// answers or the deadline lapses, whichever comes first. The GUI
/// proceeds regardless of outcome (errors are diagnostics, not
/// blockers).
pub fn ensure_stack_started() -> BootstrapOutcome {
    if std::env::var(SKIP_ENV).is_ok() {
        tracing::info!("{SKIP_ENV} set — skipping stack bootstrap");
        return BootstrapOutcome::SkippedByEnv;
    }

    if probe_health() {
        tracing::info!("backend already reachable on /api/health — skipping compose-up");
        return BootstrapOutcome::AlreadyRunning;
    }

    if !docker_available() {
        tracing::warn!(
            "docker daemon not reachable — backend isn't running and won't be auto-started"
        );
        return BootstrapOutcome::DockerUnavailable;
    }

    tracing::info!(
        compose_file = COMPOSE_FILE,
        "starting backend stack via docker compose up -d…"
    );
    let output = match Command::new("docker")
        .args(["compose", "-f", COMPOSE_FILE, "up", "-d"])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            return BootstrapOutcome::ComposeFailed {
                stderr: format!("spawn failed: {e}"),
            };
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        tracing::error!(stderr = %stderr.trim(), "docker compose up returned non-zero");
        return BootstrapOutcome::ComposeFailed { stderr };
    }
    tracing::info!("docker compose up succeeded — waiting for /api/health to answer");

    match wait_for_health() {
        Ok(()) => BootstrapOutcome::StartedViaCompose,
        Err(last) => BootstrapOutcome::StartedButNotReady { last_error: last },
    }
}

/// Run `docker compose -f <compose> down` to stop the backend
/// stack. Symmetric counterpart of [`ensure_stack_started`] —
/// called from the binary's SIGINT handler when
/// [`STOP_ON_EXIT_ENV`] is set. Synchronous; logs the outcome.
/// Idempotent — succeeds even if the stack isn't running.
pub fn stop_stack() -> Result<(), String> {
    let output = Command::new("docker")
        .args(["compose", "-f", COMPOSE_FILE, "down"])
        .output()
        .map_err(|e| format!("spawn failed: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "docker compose down: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

/// Quick `docker info` probe. Returns `true` only if the daemon
/// answers — `false` covers both "docker not on PATH" and
/// "docker is installed but the daemon isn't running".
fn docker_available() -> bool {
    Command::new("docker")
        .args(["info", "--format", "{{.ServerVersion}}"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Single shot at `/api/health` via the existing `Client`. Returns
/// `true` iff the request succeeded. `Client::new()` defaults to
/// the loopback `:7878` URL with a 5s per-request timeout — see
/// `api::Client::with_base_url`.
fn probe_health() -> bool {
    Client::new().health().is_ok()
}

/// Poll `/api/health` until success or [`READY_TIMEOUT`] elapses.
fn wait_for_health() -> Result<(), String> {
    let deadline = Instant::now() + READY_TIMEOUT;
    let client = Client::new();
    let mut last_err = String::from("no probe attempted yet");
    while Instant::now() < deadline {
        match client.health() {
            Ok(_) => return Ok(()),
            Err(e) => {
                last_err = format!("{e:#}");
                std::thread::sleep(HEALTH_PROBE_SLEEP);
            }
        }
    }
    Err(last_err)
}
