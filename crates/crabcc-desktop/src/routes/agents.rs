//! Agents route — full-detail live agents list.
//!
//! The Home dashboard renders a compact 8-row tile with agent id /
//! runtime — fine at a glance, but it loses the model, pid, prompt
//! preview, log volume, and project root. This route lifts the same
//! `AppState::agents` slice into a dedicated page with all the SSE
//! fields visible and no row cap.
//!
//! Read-only by design. Per-agent actions (kill, log tail, profile
//! lookup) live elsewhere in the kickoff initiative.

use gpui::{div, prelude::*, px, Context, Entity, IntoElement, Render, SharedString, Window};
use gpui_component::{h_flex, v_flex, ActiveTheme};

use crate::api::types::AgentStatus;
use crate::state::AppState;

pub struct AgentsRoute {
    state: Entity<AppState>,
}

impl AgentsRoute {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self { state }
    }
}

impl Render for AgentsRoute {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let foreground = cx.theme().foreground;
        let border = cx.theme().border;
        let success = cx.theme().success;
        let primary = cx.theme().primary;

        let state = self.state.read(cx);
        let total = state.agents.len();
        let running = state
            .agents
            .iter()
            .filter(|a| a.status == AgentStatus::Running)
            .count();

        // ── Header ──────────────────────────────────────────────────
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
                    .child(SharedString::new_static("Agents")),
            )
            .child(div().text_color(muted).child(SharedString::from(format!(
                "· {total} total · {running} running"
            ))));

        // ── Body ────────────────────────────────────────────────────
        let body: gpui::AnyElement = if state.agents.is_empty() {
            div()
                .px_5()
                .py_3()
                .text_color(muted)
                .child(SharedString::new_static(
                    "no agents tracked — launch one from Home or via crabcc agents",
                ))
                .into_any_element()
        } else {
            v_flex()
                .px_5()
                .py_2()
                .gap_2()
                .children(state.agents.iter().map(|a| {
                    let dot = match a.status {
                        AgentStatus::Running => "●",
                        AgentStatus::Exited => "○",
                    };
                    let dot_color = match a.status {
                        AgentStatus::Running => success,
                        AgentStatus::Exited => muted,
                    };
                    let runtime = a.runtime.clone().unwrap_or_else(|| "—".into());
                    let model = a.model.clone().unwrap_or_else(|| "—".into());
                    let pid = a.pid.map(|p| p.to_string()).unwrap_or_else(|| "—".into());
                    // Best-effort start-time formatter. We avoid pulling
                    // chrono just for this — `started_ts` is unix-seconds,
                    // and "Xs ago" is what a glance-pane wants anyway.
                    let age = relative_age(a.started_ts, state.last_event_ts);
                    let log_kib = a.log_bytes as f64 / 1024.0;
                    let root_short = a
                        .root
                        .as_ref()
                        .and_then(|r| r.rsplit('/').next())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "—".into());

                    // First row: status, id, runtime · model.
                    let head_row = h_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_color(dot_color)
                                .child(SharedString::from(dot.to_string())),
                        )
                        .child(
                            div()
                                .text_color(foreground)
                                .child(SharedString::from(a.id.clone())),
                        )
                        .child(
                            div()
                                .text_color(muted)
                                .child(SharedString::from(format!("· {runtime} · {model}"))),
                        );

                    // Second row: pid, age, log kib, root.
                    let meta_row = h_flex()
                        .gap_3()
                        .text_color(muted)
                        .child(SharedString::from(format!("pid {pid}")))
                        .child(SharedString::from(age))
                        .child(SharedString::from(format!("{log_kib:.1} KiB log")))
                        .child(SharedString::from(format!("root: {root_short}")));

                    // Optional prompt-preview row. Empty when the agent
                    // didn't carry one (e.g. legacy launches).
                    let prompt_row: gpui::AnyElement = if a.prompt_preview.trim().is_empty() {
                        div().into_any_element()
                    } else {
                        div()
                            .text_color(primary)
                            .child(SharedString::from(format!(
                                "\u{201C}{}\u{201D}",
                                a.prompt_preview.clone()
                            )))
                            .into_any_element()
                    };

                    v_flex()
                        .gap_1()
                        .px_3()
                        .py_2()
                        .border_1()
                        .border_color(border)
                        .rounded_md()
                        .child(head_row)
                        .child(meta_row)
                        .child(prompt_row)
                        .into_any_element()
                }))
                .into_any_element()
        };

        v_flex()
            .size_full()
            .child(header)
            .child(div().flex_1().min_h(px(0.0)).child(body))
    }
}

/// Cheap "Xs ago" formatter — uses `last_event_ts` as the clock proxy
/// to avoid adding a real time crate just for one display string. The
/// drift vs wall-clock is at most one SSE poll interval, which is
/// invisible at this granularity.
fn relative_age(started_ts: i64, now_ts: Option<i64>) -> String {
    let Some(now) = now_ts else {
        return "—".into();
    };
    if started_ts == 0 {
        return "—".into();
    }
    let secs = (now - started_ts).max(0);
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}
