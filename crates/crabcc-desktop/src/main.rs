// Phase A.6 — multi-route shell mounted in the gpui window.
//
// `main` opens a 1280×800 window, builds the shared `AppState` entity
// (prefetch + SSE workers), and mounts `crabcc_desktop::shell::Shell`
// as the root view. Shell owns the header + nav strip and dispatches
// the body slot based on `AppState::route`.

use crabcc_desktop::api::DEFAULT_BASE_URL;
use crabcc_desktop::shell::Shell;
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
                let shell = cx.new(|cx| Shell::new(app_state, cx));
                cx.new(|cx| Root::new(shell, window, cx))
            })
            .expect("failed to open window");
        })
        .detach();
    });
}
