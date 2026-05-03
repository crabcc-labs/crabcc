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
    match services::ensure_stack_started() {
        BootstrapOutcome::SkippedByEnv => {
            tracing::info!("backend bootstrap skipped via env var");
        }
        BootstrapOutcome::AlreadyRunning => {
            tracing::info!("backend already reachable — bootstrap was a no-op");
        }
        BootstrapOutcome::StartedViaCompose => {
            tracing::info!("backend started via docker compose, ready");
        }
        BootstrapOutcome::StartedButNotReady { last_error } => {
            tracing::warn!(error = %last_error, "compose up succeeded but backend didn't answer in time");
        }
        BootstrapOutcome::DockerUnavailable => {
            tracing::warn!("docker daemon unavailable — backend won't be auto-started; install docker or run `crabcc serve` manually");
        }
        BootstrapOutcome::ComposeFailed { stderr } => {
            tracing::error!(stderr = %stderr.trim(), "docker compose up failed");
        }
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

        cx.spawn(async move |cx| {
            cx.open_window(options, |window, cx| {
                let app_state = cx.new(|cx| state::build(cx, DEFAULT_BASE_URL));
                let shell = cx.new(|cx| Shell::new(app_state, window, cx));
                cx.new(|cx| Root::new(shell, window, cx))
            })
            .expect("failed to open window");
        })
        .detach();
    });
}
