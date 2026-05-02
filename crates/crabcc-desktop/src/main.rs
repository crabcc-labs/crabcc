// Phase A.4 — DashboardHome route mounted in the gpui window.
//
// `main` is now intentionally thin: open a 1280×800 window, build the
// shared `AppState` entity (which spawns the prefetch + SSE workers),
// and mount `routes::dashboard::DashboardHome` as the root view.
// State, rendering, and worker plumbing all live in their own modules
// — see `state.rs` and `routes/dashboard.rs`.
//
// Until the multi-route header lands in A.6, the window only ever
// shows the home route. A.5 will swap in a `Stack`-based router.

use crabcc_desktop::api::DEFAULT_BASE_URL;
use crabcc_desktop::routes::dashboard::DashboardHome;
use crabcc_desktop::state;
use gpui::{prelude::*, px, size, App, Bounds, TitlebarOptions, WindowBounds, WindowOptions};
use gpui_component::Root;

const WINDOW_TITLE: &str = "crabcc · live";

fn main() {
    gpui_platform::application().run(move |cx: &mut App| {
        gpui_component::init(cx);

        let bounds = Bounds::centered(None, size(px(1280.0), px(800.0)), cx);

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
                let dashboard = cx.new(|cx| DashboardHome::new(app_state, cx));
                cx.new(|cx| Root::new(dashboard, window, cx))
            })
            .expect("failed to open window");
        })
        .detach();
    });
}
