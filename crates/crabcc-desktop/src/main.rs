// Phase A.1 — minimal GPUI shell. Just opens a 1280×800 window with a
// static panel. No API client, no SSE, no theme yet — those land in
// A.2 / A.3. The job here is to prove that gpui + gpui-component
// compile, link, and render against our pinned upstream rev.
//
// See docs/RESEARCH-native-desktop-and-rich-notifications.md (Track A)
// for the full phasing.

use gpui::{
    div, prelude::*, px, size, App, Bounds, Context, IntoElement, Render, SharedString,
    TitlebarOptions, Window, WindowBounds, WindowOptions,
};
use gpui_component::{h_flex, v_flex, Root};

const WINDOW_TITLE: &str = "crabcc · live";

struct Shell {
    repo: SharedString,
    version: SharedString,
}

impl Render for Shell {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_4()
            .child(
                div()
                    .text_2xl()
                    .child(SharedString::new_static("crabcc · live")),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(SharedString::new_static("v"))
                    .child(self.version.clone()),
            )
            .child(self.repo.clone())
    }
}

fn main() {
    let app = gpui_platform::application();

    app.run(move |cx: &mut App| {
        gpui_component::init(cx);

        let bounds = Bounds::centered(
            None,
            size(px(1280.0), px(800.0)),
            cx,
        );

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
                let shell = cx.new(|_| Shell {
                    repo: env!("CARGO_PKG_REPOSITORY").into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                });
                cx.new(|cx| Root::new(shell, window, cx))
            })
            .expect("failed to open window");
        })
        .detach();
    });
}
