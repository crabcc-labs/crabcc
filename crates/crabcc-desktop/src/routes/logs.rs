//! Logs route — placeholder until the telemetry / log-tail viewer ports
//! across from `crates/crabcc-viz/web/src/components/TelemetryPanel.tsx`.
//! Lands as part of A.6 — see the TODO list on the kickoff PR.

use gpui::{div, prelude::*, px, Context, IntoElement, Render, SharedString, Window};
use gpui_component::{v_flex, ActiveTheme};

pub struct LogsRoute;

impl LogsRoute {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LogsRoute {
    fn default() -> Self {
        Self::new()
    }
}

impl Render for LogsRoute {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .px_5()
            .py_4()
            .gap_2()
            .child(
                div()
                    .text_lg()
                    .child(SharedString::new_static("Logs")),
            )
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .min_h(px(60.0))
                    .child(SharedString::new_static(
                        "Telemetry tail + level filters land in a follow-up. \
                         For now, the API client already has typed access to \
                         /api/telemetry — see crabcc_desktop::api::Client::telemetry.",
                    )),
            )
    }
}
