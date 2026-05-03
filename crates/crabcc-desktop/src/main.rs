// Phase A.6 — multi-route shell mounted in the gpui window.
//
// `main` opens a 1600×1000 window, builds the shared `AppState` entity
// (prefetch + SSE workers), and mounts `crabcc_desktop::shell::Shell`
// as the root view. Shell owns the header + nav strip and dispatches
// the body slot based on `AppState::route`. The body is wrapped in
// an `overflow_y_scroll` container so dense routes (System, Knowledge,
// Logs) and a tall Home dashboard scroll instead of clipping.

use crabcc_desktop::api::DEFAULT_BASE_URL;
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
