//! System route — placeholder. Will surface service-discovery detail
//! (latency, source env-var, error trail), OTLP health, and ollama-key
//! drawer rows once the layout settles. Pulls from the same
//! `AppState.services` already populated by prefetch.

use gpui::{div, prelude::*, px, Context, Entity, IntoElement, Render, SharedString, Window};
use gpui_component::{h_flex, v_flex, ActiveTheme};

use crate::state::AppState;

pub struct SystemRoute {
    state: Entity<AppState>,
}

impl SystemRoute {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self { state }
    }
}

impl Render for SystemRoute {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let muted = cx.theme().muted_foreground;

        let services_block: gpui::AnyElement = match state.services.as_ref() {
            None => div()
                .text_color(muted)
                .child(SharedString::new_static("loading services…"))
                .into_any_element(),
            Some(rep) => v_flex()
                .gap_1()
                .children(rep.services.iter().map(|s| {
                    let mark = if s.reachable { "✓" } else { "✗" };
                    h_flex()
                        .gap_3()
                        .child(div().w(px(20.0)).child(SharedString::from(mark.to_string())))
                        .child(div().w(px(160.0)).child(SharedString::from(s.name.clone())))
                        .child(
                            div()
                                .w(px(80.0))
                                .text_color(muted)
                                .child(SharedString::from(format!("{}ms", s.latency_ms))),
                        )
                        .child(div().text_color(muted).child(SharedString::from(s.url.clone())))
                        .into_any_element()
                }))
                .into_any_element(),
        };

        v_flex()
            .size_full()
            .px_5()
            .py_4()
            .gap_3()
            .child(div().text_lg().child(SharedString::new_static("System")))
            .child(services_block)
    }
}
