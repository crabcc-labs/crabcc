//! DashboardHome — Milestone 1.
//!
//! Visual layout:
//!
//!   ┌─ titlebar (set by gpui at window-open time) ─────────────┐
//!   │  KPI strip: [Index] [Activity] [Agents] [Services]       │
//!   ├──────────────────────────────────────────────────────────┤
//!   │  Tile row: [Recent activity] [Agents] [Services]         │
//!   └──────────────────────────────────────────────────────────┘
//!
//! Reads from the shared `AppState` entity. `Render` runs on every
//! `cx.notify()` triggered by the SSE pump in `state.rs`.

use gpui::{
    div, prelude::*, px, Context, Entity, IntoElement, Render, SharedString, Window,
};
use gpui_component::{h_flex, v_flex, ActiveTheme};

use crate::api::types::AgentStatus;
use crate::state::AppState;

pub struct DashboardHome {
    state: Entity<AppState>,
}

impl DashboardHome {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        // Re-render this view whenever AppState publishes a notify.
        // gpui doesn't propagate the source entity's notifications
        // automatically — we observe explicitly.
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self { state }
    }
}

impl Render for DashboardHome {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);

        let bg = cx.theme().background;
        // gpui-component uses `secondary` for elevated panels — there's
        // no shadcn-style `card` token in this theme. Re-evaluate when
        // we adopt a `Card` component (track A.5+).
        let card = cx.theme().secondary;
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;

        let header = h_flex()
            .gap_3()
            .px_5()
            .py_3()
            .border_b_1()
            .border_color(border)
            .child(
                div()
                    .text_lg()
                    .text_color(cx.theme().foreground)
                    .child(SharedString::new_static("crabcc · live")),
            )
            .child(
                div().text_color(muted).child(SharedString::from(
                    state
                        .bootstrap
                        .as_ref()
                        .map(|b| format!("v{}  {}", b.version, b.repo))
                        .unwrap_or_else(|| "loading…".into()),
                )),
            );

        // ── KPI strip ─────────────────────────────────────────────
        let index_kpi = match state.bootstrap.as_ref().and_then(|b| b.index.as_ref()) {
            Some(idx) => format!(
                "{} files · {} symbols",
                idx.files.unwrap_or(0),
                idx.symbols.unwrap_or(0)
            ),
            None => "—".into(),
        };

        let activity_kpi = format!("{} hits", state.activity_total);
        let agents_kpi = format!(
            "{}/{} running",
            state.agents_running(),
            state.agents.len()
        );
        let services_kpi = state
            .services_reachable()
            .map(|(up, total)| format!("{up}/{total} reachable"))
            .unwrap_or_else(|| "—".into());

        let kpi_strip = h_flex()
            .gap_3()
            .px_5()
            .py_4()
            .child(kpi_card("INDEX", index_kpi, card, border, muted))
            .child(kpi_card("ACTIVITY", activity_kpi, card, border, muted))
            .child(kpi_card("AGENTS", agents_kpi, card, border, muted))
            .child(kpi_card("SERVICES", services_kpi, card, border, muted));

        // ── Tile row ──────────────────────────────────────────────
        let activity_tile = tile(
            "Recent activity",
            card,
            border,
            v_flex().gap_1().children(
                state
                    .recent_activity
                    .iter()
                    .rev()
                    .take(8)
                    .map(|hit| {
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .w(px(80.0))
                                    .text_color(muted)
                                    .child(SharedString::from(hit.op.clone())),
                            )
                            .child(SharedString::from(truncate(&hit.query, 60)))
                            .child(
                                div()
                                    .text_color(muted)
                                    .child(SharedString::from(format!("({})", hit.results))),
                            )
                            .into_any_element()
                    })
                    .collect::<Vec<_>>(),
            ),
        );

        let agents_tile = tile(
            "Agents",
            card,
            border,
            v_flex().gap_1().children(
                state
                    .agents
                    .iter()
                    .take(8)
                    .map(|a| {
                        let dot = match a.status {
                            AgentStatus::Running => "●",
                            AgentStatus::Exited => "○",
                        };
                        h_flex()
                            .gap_2()
                            .child(SharedString::from(dot.to_string()))
                            .child(SharedString::from(a.id.clone()))
                            .child(
                                div().text_color(muted).child(SharedString::from(
                                    a.runtime
                                        .clone()
                                        .unwrap_or_else(|| "—".to_string()),
                                )),
                            )
                            .into_any_element()
                    })
                    .collect::<Vec<_>>(),
            ),
        );

        let services_tile = tile(
            "Services",
            card,
            border,
            v_flex().gap_1().children(match state.services.as_ref() {
                Some(rep) => rep
                    .services
                    .iter()
                    .take(10)
                    .map(|s| {
                        let mark = if s.reachable { "✓" } else { "✗" };
                        h_flex()
                            .gap_2()
                            .child(SharedString::from(mark.to_string()))
                            .child(SharedString::from(s.name.clone()))
                            .child(
                                div()
                                    .text_color(muted)
                                    .child(SharedString::from(format!("{}ms", s.latency_ms))),
                            )
                            .into_any_element()
                    })
                    .collect::<Vec<_>>(),
                None => vec![div()
                    .text_color(muted)
                    .child(SharedString::new_static("loading…"))
                    .into_any_element()],
            }),
        );

        let tile_row = h_flex()
            .gap_3()
            .px_5()
            .py_2()
            .child(activity_tile)
            .child(agents_tile)
            .child(services_tile);

        v_flex()
            .size_full()
            .bg(bg)
            .child(header)
            .child(kpi_strip)
            .child(tile_row)
    }
}

fn kpi_card(
    label: &'static str,
    value: String,
    card_bg: gpui::Hsla,
    border: gpui::Hsla,
    muted: gpui::Hsla,
) -> gpui::Div {
    v_flex()
        .min_w(px(180.0))
        .p_3()
        .gap_1()
        .bg(card_bg)
        .border_1()
        .border_color(border)
        .rounded_md()
        .child(
            div()
                .text_xs()
                .text_color(muted)
                .child(SharedString::new_static(label)),
        )
        .child(div().text_xl().child(SharedString::from(value)))
}

fn tile(
    title: &'static str,
    card_bg: gpui::Hsla,
    border: gpui::Hsla,
    body: impl IntoElement,
) -> gpui::Div {
    v_flex()
        .flex_1()
        .min_h(px(220.0))
        .p_3()
        .gap_2()
        .bg(card_bg)
        .border_1()
        .border_color(border)
        .rounded_md()
        .child(
            div()
                .text_sm()
                .child(SharedString::new_static(title)),
        )
        .child(body)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}
