//! Knowledge route — placeholder for the memory-drawer + knowledge-graph
//! view. Wire-side surface (`/api/memory/*` + `KnowledgeView` on the
//! React side) is mature; the gpui port lands in a follow-up.

use gpui::{div, prelude::*, px, Context, IntoElement, Render, SharedString, Window};
use gpui_component::{v_flex, ActiveTheme};

pub struct KnowledgeRoute;

impl KnowledgeRoute {
    pub fn new() -> Self {
        Self
    }
}

impl Default for KnowledgeRoute {
    fn default() -> Self {
        Self::new()
    }
}

impl Render for KnowledgeRoute {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .px_5()
            .py_4()
            .gap_2()
            .child(
                div()
                    .text_lg()
                    .child(SharedString::new_static("Knowledge")),
            )
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .min_h(px(60.0))
                    .child(SharedString::new_static(
                        "Memory drawer + knowledge-graph viewer land in a follow-up. \
                         The React surface (KnowledgeView + IngestBox) is the reference \
                         layout; until then `crabcc memory ingest` over HTTP from the CLI \
                         remains the way to add drawers.",
                    )),
            )
    }
}
