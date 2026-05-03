// Phase A.6 — multi-route shell mounted in the gpui window.
//
// `main` opens a 1600×1000 window, builds the shared `AppState` entity
// (prefetch + SSE workers), and mounts `crabcc_desktop::shell::Shell`
// as the root view. Shell owns the header + nav strip and dispatches
// the body slot based on `AppState::route`. The body is wrapped in
// an `overflow_y_scroll` container so dense routes (System, Knowledge,
// Logs) and a tall Home dashboard scroll instead of clipping.

use crabcc_desktop::api::DEFAULT_BASE_URL;
use crabcc_desktop::services::{self, BootstrapOutcome};
use crabcc_desktop::shell::Shell;
use crabcc_desktop::state;
use crabcc_desktop::toasts::ToastLevel;
use gpui::{prelude::*, px, size, App, Bounds, TitlebarOptions, WindowBounds, WindowOptions};
use gpui_component::Root;

const WINDOW_TITLE: &str = "crabcc · live";

fn main() {
    // Structured logging — defaults to `info` if `RUST_LOG` isn't set.
    // Devs can crank up SSE / state lifecycle visibility via:
    //   RUST_LOG=crabcc_desktop=debug
    // Pre-cursor to the QUIC migration work in #239: path-change races
    // and the WiFi→cellular handover need observable signal, not
    // eprintlns. The subscriber registers cheaply when no spans are
    // recorded (typical user run with default `info`).
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(true)
        .init();

    // Ensure the docker-compose backend stack is up before we open
    // the window. Synchronous; non-fatal — the GUI proceeds even if
    // bootstrap fails (slice 3's prefetch Danger toast surfaces the
    // specific reachability error). Set `CRABCC_DESKTOP_SKIP_SERVICES`
    // to opt out (devs running their own stack from another shell).
    let (outcome, elapsed) = services::ensure_stack_started();
    let bootstrap_toast = bootstrap_outcome_to_toast(outcome, elapsed);

    // Optional graceful-shutdown counterpart. Off by default — most
    // users have multiple consumers of the stack (web dashboard,
    // agents, telegram bot) and tearing it down on every desktop
    // quit would surprise them. Opt-in via
    // `CRABCC_DESKTOP_STOP_SERVICES_ON_EXIT=1`. Catches SIGINT only;
    // GPUI's window-close (cmd-Q) doesn't currently fire this path.
    if std::env::var(services::STOP_ON_EXIT_ENV).is_ok() {
        let _ = ctrlc::set_handler(|| {
            tracing::info!("SIGINT received — stopping backend stack via docker compose down");
            match services::stop_stack() {
                Ok(()) => tracing::info!("backend stack stopped"),
                Err(e) => tracing::error!(error = %e, "docker compose down failed on shutdown"),
            }
            std::process::exit(0);
        });
    }

    gpui_platform::application().run(move |cx: &mut App| {
        gpui_component::init(cx);

        // Default to a roomier window than A.6's 1280×800 so the
        // dashboard's KPI strip + 3-tile row + spawn row + relations
        // graph fit without scrolling on a typical 14"+ laptop or
        // external monitor. Users on smaller screens still get a
        // scrollable body (see `Shell::render`), so this bigger
        // default only widens the no-scroll happy path.
        let bounds = Bounds::centered(None, size(px(1600.0), px(1000.0)), cx);

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some(WINDOW_TITLE.into()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let bootstrap_toast = bootstrap_toast.clone();
        cx.spawn(async move |cx| {
            let bootstrap_toast = bootstrap_toast.clone();
            cx.open_window(options, move |window, cx| {
                let app_state = cx.new(|cx| state::build(cx, DEFAULT_BASE_URL));
                // Pre-seed the toast strip with the services
                // bootstrap outcome — so the user sees Docker /
                // health problems on the live surface, not just in
                // stderr. `AlreadyRunning` is the silent happy-path
                // and contributes no toast; the other variants pop
                // an Info / Success / Danger row on first render.
                if let Some((level, msg)) = bootstrap_toast {
                    app_state.update(cx, |s, _| {
                        s.push_toast(level, msg);
                    });
                }
                let shell = cx.new(|cx| Shell::new(app_state, window, cx));
                cx.new(|cx| Root::new(shell, window, cx))
            })
            .expect("failed to open window");
        })
        .detach();
    });
}

/// Convert a `BootstrapOutcome` into an optional toast pair, while
/// emitting a structured `tracing` log for every variant. Returns
/// `None` for the silent happy-paths (`AlreadyRunning` — operator
/// doesn't need to know nothing happened). The duration is
/// rendered into the messages where it's interesting (i.e.
/// happy-path "started in 2.3s" and the not-ready failure where
/// "we waited Xs and gave up" is the relevant signal).
fn bootstrap_outcome_to_toast(
    outcome: BootstrapOutcome,
    elapsed: std::time::Duration,
) -> Option<(ToastLevel, String)> {
    let secs = elapsed.as_secs_f32();
    match outcome {
        BootstrapOutcome::SkippedByEnv => {
            tracing::info!("backend bootstrap skipped via env var");
            Some((ToastLevel::Info, "backend bootstrap skipped (env)".into()))
        }
        BootstrapOutcome::AlreadyRunning => {
            tracing::info!(
                elapsed_secs = secs,
                "backend already reachable — bootstrap was a no-op"
            );
            None
        }
        BootstrapOutcome::StartedViaCompose => {
            tracing::info!(
                elapsed_secs = secs,
                "backend started via docker compose, ready"
            );
            Some((
                ToastLevel::Success,
                format!("backend started via docker compose in {secs:.1}s"),
            ))
        }
        BootstrapOutcome::StartedButNotReady { last_error } => {
            tracing::warn!(error = %last_error, elapsed_secs = secs, "compose up succeeded but backend didn't answer in time");
            Some((
                ToastLevel::Danger,
                format!("backend not responding after {secs:.1}s: {last_error}"),
            ))
        }
        BootstrapOutcome::DockerUnavailable => {
            tracing::warn!("docker daemon unavailable — backend won't be auto-started; install docker or run `crabcc serve` manually");
            Some((
                ToastLevel::Danger,
                "docker daemon unavailable — start crabcc serve manually".into(),
            ))
        }
        BootstrapOutcome::ComposeFailed { stderr } => {
            tracing::error!(stderr = %stderr.trim(), elapsed_secs = secs, "docker compose up failed");
            Some((
                ToastLevel::Danger,
                format!("docker compose up failed: {}", stderr.trim()),
            ))
        }
    }
}
