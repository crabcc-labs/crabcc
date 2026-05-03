//! Knowledge graph canvas — slot for the second graph view (#297).
//!
//! Distinct from the Home dashboard's relations graph: nodes are
//! memory drawers, edges are wiki-style cross-references between
//! them. The brief calls for a Roam-like map with wing-coloured
//! pills, dashed cross-ref edges, and a right-rail Drawer Detail
//! panel.
//!
//! **This is a stub.** The desktop client doesn't have the data to
//! render the canvas — `MemoryRecentResponse` carries individual
//! drawers but no cross-reference edges. A server-side
//! `/api/memory/graph` endpoint that emits `{ nodes: [...drawers],
//! edges: [...cross_refs] }` is required and tracked as a separate
//! follow-up. Until it lands, this route shows:
//!
//!   * The current drawer-count + wing summary (data we already
//!     have, mirrored from the Knowledge route).
//!   * An explicit gap notice with the follow-up issue link.
//!
//! Owning a route slot now keeps the nav strip honest about the
//! design brief's surface inventory and gives the next implementer
//! a clear handle to pick up.

use std::collections::HashMap;

use gpui::{div, prelude::*, px, Context, Entity, Hsla, IntoElement, Render, SharedString, Window};
use gpui_component::{h_flex, v_flex, ActiveTheme};

use crate::state::AppState;

pub struct KnowledgeGraphRoute {
    state: Entity<AppState>,
}

impl KnowledgeGraphRoute {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self { state }
    }
}

impl Render for KnowledgeGraphRoute {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let foreground = theme.foreground;
        let border = theme.border;
        let warning = theme.warning;
        let primary = theme.primary;
        let secondary = theme.secondary;

        let state = self.state.read(cx);
        let drawer_count = state
            .memory_recent
            .as_ref()
            .map(|r| r.drawers.len())
            .unwrap_or(0);
        let backend_present = state
            .memory_recent
            .as_ref()
            .map(|r| r.present)
            .unwrap_or(false);
        // Wing tally — same shape the Knowledge route's summary line
        // uses, lifted here so the user sees that the route knows
        // about the data, just not yet how to lay it out.
        let mut wings: HashMap<SharedString, usize> = HashMap::new();
        if let Some(r) = state.memory_recent.as_ref() {
            for d in &r.drawers {
                *wings.entry(d.wing.clone()).or_insert(0) += 1;
            }
        }

        let mut wing_pairs: Vec<(SharedString, usize)> = wings.into_iter().collect();
        wing_pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.as_ref().cmp(b.0.as_ref())));

        let header = h_flex()
            .gap_3()
            .px_5()
            .py_3()
            .border_b_1()
            .border_color(border)
            .child(
                div()
                    .text_lg()
                    .text_color(foreground)
                    .child(SharedString::new_static("Knowledge graph")),
            )
            .child(
                div()
                    .text_color(muted)
                    .child(SharedString::from(if backend_present {
                        format!("· {drawer_count} drawers loaded")
                    } else {
                        "· memory backend not bootstrapped (run `crabcc memory init`)".into()
                    })),
            );

        // Server-side gap — make the missing endpoint impossible to
        // miss. Warning colour, prominent placement, follow-up issue
        // link in plain text so a contributor reading the in-app
        // surface can find it without leaving.
        let gap_block = v_flex()
            .gap_2()
            .mx_5()
            .mt_4()
            .p_4()
            .border_1()
            .border_color(warning)
            .rounded_md()
            .bg(secondary)
            .child(
                div()
                    .text_color(warning)
                    .child(SharedString::new_static(
                        "⚠ blocked on server-side `/api/memory/graph` endpoint",
                    )),
            )
            .child(
                div()
                    .text_color(muted)
                    .text_xs()
                    .child(SharedString::new_static(
                        "MemoryRecentResponse carries drawers but no cross-reference edges. \
                         Once the server emits a graph payload (nodes + edges), this route \
                         will render a Roam-like canvas distinct from the relations graph.",
                    )),
            )
            .child(
                div()
                    .text_color(primary)
                    .text_xs()
                    .child(SharedString::new_static(
                        "track: feat(server,memory): expose memory cross-reference graph at /api/memory/graph",
                    )),
            );

        // Wing tally block — mirrors what the Knowledge route's
        // summary line does, so the user can see the underlying
        // material that *is* available even though the canvas can't
        // render it yet.
        let mut wing_rows = v_flex().gap_1().mx_5().mt_4().child(
            div()
                .text_color(muted)
                .text_xs()
                .child(SharedString::new_static("WINGS (drawer count)")),
        );
        if wing_pairs.is_empty() {
            wing_rows = wing_rows.child(
                div()
                    .text_color(muted)
                    .text_xs()
                    .child(SharedString::new_static("no drawers loaded yet")),
            );
        } else {
            for (name, count) in wing_pairs {
                wing_rows =
                    wing_rows.child(wing_row(name.to_string(), count, muted, foreground, border));
            }
        }

        v_flex()
            .size_full()
            .child(header)
            .child(gap_block)
            .child(wing_rows)
    }
}

fn wing_row(name: String, count: usize, muted: Hsla, foreground: Hsla, border: Hsla) -> gpui::Div {
    h_flex()
        .gap_3()
        .px_2()
        .py_0p5()
        .border_b_1()
        .border_color(border)
        .child(
            div()
                .min_w(px(180.0))
                .text_color(foreground)
                .child(SharedString::from(name)),
        )
        .child(
            div()
                .text_color(muted)
                .child(SharedString::from(format!("{count} drawers"))),
        )
}
