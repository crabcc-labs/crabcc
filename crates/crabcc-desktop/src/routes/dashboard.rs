//! DashboardHome — body content for the Home route.
//!
//! Layout (header + nav owned by `crate::shell`):
//!
//!   KPI strip   [Index] [Activity] [Agents] [Services]
//!   Tile row    [Recent activity] [Agents] [Services]
//!   Spawn row   Launch agent — prompt input + button + status
//!   Graph row   Relations graph (canvas, ≥360px tall)
//!
//! Reads from the shared `AppState` entity. `Render` runs on every
//! `cx.notify()` triggered by the SSE pump in `state.rs`.

use gpui::{
    div, prelude::*, px, Context, Entity, Hsla, IntoElement, MouseButton, Render, SharedString,
    Window,
};
use gpui_component::{
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex, ActiveTheme,
};

use crate::api::types::{AgentLaunchRequest, AgentStatus, SseActivityEvent};
use crate::routes::graph::GraphView;
use crate::state::AppState;

pub struct DashboardHome {
    state: Entity<AppState>,
    graph_view: Entity<GraphView>,
    /// Agent-spawn form input — placed between the tile row and the
    /// graph so it stays visible without scrolling.
    spawn_input: Entity<InputState>,
    /// Live mirror of the spawn input's text — read on submit.
    spawn_text: String,
}

impl DashboardHome {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        let graph_view = cx.new(|cx| GraphView::new(state.clone(), cx));
        let spawn_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Spawn agent: prompt…")
        });
        cx.subscribe_in(&spawn_input, window, |this, state, event, _, cx| {
            match event {
                InputEvent::Change => {
                    this.spawn_text = state.read(cx).value().to_string();
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.submit_launch(cx),
                _ => {}
            }
        })
        .detach();
        Self {
            state,
            graph_view,
            spawn_input,
            spawn_text: String::new(),
        }
    }

    fn submit_launch(&mut self, cx: &mut Context<Self>) {
        let prompt = self.spawn_text.trim();
        if prompt.is_empty() {
            return;
        }
        let req = AgentLaunchRequest {
            prompt: prompt.to_string(),
            ..Default::default()
        };
        self.state.read(cx).submit_launch(req);
        cx.notify();
    }
}

impl Render for DashboardHome {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);

        // gpui-component uses `secondary` for elevated panels — there's
        // no shadcn-style `card` token in this theme. Re-evaluate when
        // we adopt a `Card` component (track A.5+).
        let card = cx.theme().secondary;
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;

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
        // Groups consecutive same-op rows into a single visual line so
        // a burst of the same query (common during a startup outline
        // sweep) doesn't drown out the variety. Op badge is colour-coded
        // per family — see `op_color`.
        let theme = cx.theme();
        let activity_groups = group_activity(&state.recent_activity, 8);
        let activity_tile = tile(
            "Recent activity",
            card,
            border,
            v_flex().gap_1().children(
                activity_groups
                    .into_iter()
                    .map(|g| {
                        let op_color = op_color(&g.op, theme);
                        h_flex()
                            .gap_2()
                            // Op badge — fixed-width column so the
                            // query text aligns across rows.
                            .child(
                                div()
                                    .w(px(80.0))
                                    .text_color(op_color)
                                    .child(SharedString::from(g.op.clone())),
                            )
                            .child(SharedString::from(truncate(&g.latest_query, 60)))
                            .child(
                                div()
                                    .text_color(muted)
                                    .child(SharedString::from(if g.count == 1 {
                                        format!("({})", g.latest_results)
                                    } else {
                                        format!("(×{} · {})", g.count, g.latest_results)
                                    })),
                            )
                            .into_any_element()
                    })
                    .collect::<Vec<_>>(),
            ),
        );

        // Agents tile gets a per-row Kill button for *running* agents.
        // Exited rows just show the dot + id + runtime; clicking nothing
        // useful would be misleading. Each Kill button captures the
        // agent id by clone and dispatches `submit_kill` through the
        // shared `AppState` entity.
        let danger = cx.theme().danger;
        let agents_state = self.state.clone();
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
                        let kill_btn: gpui::AnyElement = if matches!(a.status, AgentStatus::Running)
                        {
                            let id_for_click = a.id.clone();
                            let state_for_click = agents_state.clone();
                            // Each clickable div needs a unique gpui ElementId
                            // — derive from the agent id so re-renders don't
                            // collide.
                            let element_id: gpui::ElementId =
                                SharedString::from(format!("kill-{}", a.id)).into();
                            div()
                                .id(element_id)
                                .px_2()
                                .py_0p5()
                                .border_1()
                                .border_color(danger)
                                .rounded_md()
                                .text_color(danger)
                                .child(SharedString::new_static("Kill"))
                                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                    state_for_click.read(cx).submit_kill(id_for_click.clone());
                                })
                                .into_any_element()
                        } else {
                            div().into_any_element()
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
                            .child(div().flex_1())
                            .child(kill_btn)
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

        // ── Spawn-agent row ────────────────────────────────────────
        let primary = cx.theme().primary;
        let success = cx.theme().success;
        let danger = cx.theme().danger;
        let submit_disabled = self.spawn_text.trim().is_empty();
        let submit_color = if submit_disabled { muted } else { primary };
        let view_entity = cx.entity();
        let launch_btn = div()
            .id("agent-launch-submit")
            .px_3()
            .py_1()
            .border_1()
            .border_color(submit_color)
            .rounded_md()
            .text_color(submit_color)
            .child(SharedString::new_static("Launch"))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                view_entity.update(cx, |this, cx| this.submit_launch(cx));
            });
        let status_line: gpui::AnyElement = match state.last_launch.as_ref() {
            None => div().into_any_element(),
            Some(Ok(msg)) => div()
                .text_color(success)
                .child(SharedString::from(msg.clone()))
                .into_any_element(),
            Some(Err(msg)) => div()
                .text_color(danger)
                .child(SharedString::from(msg.clone()))
                .into_any_element(),
        };
        let spawn_row = v_flex()
            .px_5()
            .py_2()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .border_1()
                            .border_color(border)
                            .rounded_md()
                            .px_2()
                            .py_1()
                            .child(Input::new(&self.spawn_input).appearance(false)),
                    )
                    .child(launch_btn),
            )
            .child(status_line);

        let graph_row = div().px_5().py_2().child(self.graph_view.clone());

        v_flex()
            .size_full()
            .child(kpi_strip)
            .child(tile_row)
            .child(spawn_row)
            .child(graph_row)
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

/// One visible row in the Recent Activity tile after consecutive
/// same-op events have been collapsed.
struct ActivityGroup {
    op: String,
    latest_query: String,
    latest_results: u64,
    count: usize,
}

/// Walk the buffer newest-first, collapsing runs of the same `op`
/// into a single group whose `count` carries the run length and
/// whose `latest_*` fields show the most-recent event in the run.
/// Returns up to `cap` groups.
fn group_activity<'a, I>(events: I, cap: usize) -> Vec<ActivityGroup>
where
    I: IntoIterator<Item = &'a SseActivityEvent>,
    I::IntoIter: DoubleEndedIterator,
{
    let mut out: Vec<ActivityGroup> = Vec::with_capacity(cap);
    for evt in events.into_iter().rev() {
        if let Some(last) = out.last_mut() {
            if last.op == evt.op {
                // Same op as the previous-newest group — extend it.
                // We already stored the *latest* (newest) event of the
                // run in `latest_*` since that came first in our walk.
                last.count += 1;
                continue;
            }
        }
        if out.len() == cap {
            break;
        }
        out.push(ActivityGroup {
            op: evt.op.clone(),
            latest_query: evt.query.clone(),
            latest_results: evt.results,
            count: 1,
        });
    }
    out
}

/// Map an op family to a theme colour. Mirrors the rough cost/value
/// hierarchy of crabcc operations: structural lookups (sym/refs/
/// callers) get bright primary/info/warning; outline (cheap, often
/// fired in bulk) stays muted; success-coloured ops are
/// non-destructive discovery (fuzzy / prefix / random-query); ingest
/// gets the primary highlight because it writes state.
fn op_color(op: &str, theme: &gpui_component::Theme) -> Hsla {
    match op {
        "sym" => theme.primary,
        "refs" => theme.info,
        "callers" => theme.warning,
        "fuzzy" | "prefix" | "random-query" => theme.success,
        "ingest" | "memory.ingest" => theme.primary,
        // Default for `outline`, `track`, and anything new we haven't
        // categorised yet — these dominate row volume and shouldn't
        // pull the eye.
        _ => theme.muted_foreground,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evt(op: &str, q: &str, results: u64) -> SseActivityEvent {
        SseActivityEvent {
            ts: 0,
            op: op.into(),
            query: q.into(),
            results,
        }
    }

    #[test]
    fn grouping_collapses_consecutive_runs() {
        // Buffer is oldest→newest; group_activity walks newest-first.
        let events = vec![
            evt("outline", "a", 1),
            evt("outline", "b", 2),
            evt("outline", "c", 3),
            evt("sym", "Store", 1),
            evt("refs", "Store", 2),
            evt("refs", "Index", 3),
        ];
        let groups = group_activity(&events, 8);
        // Expected (newest first): refs ×2 (latest=Index), sym ×1, outline ×3 (latest=c)
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].op, "refs");
        assert_eq!(groups[0].count, 2);
        assert_eq!(groups[0].latest_query, "Index");
        assert_eq!(groups[1].op, "sym");
        assert_eq!(groups[1].count, 1);
        assert_eq!(groups[2].op, "outline");
        assert_eq!(groups[2].count, 3);
        assert_eq!(groups[2].latest_query, "c");
    }

    #[test]
    fn grouping_caps_at_visible_count() {
        let events: Vec<SseActivityEvent> = (0..20)
            .map(|i| evt(&format!("op-{i}"), "q", i as u64))
            .collect();
        let groups = group_activity(&events, 5);
        // Each event has a unique op, so groups equal events. We expect
        // exactly 5 (the cap) — the *newest* 5.
        assert_eq!(groups.len(), 5);
        assert_eq!(groups[0].op, "op-19");
        assert_eq!(groups[4].op, "op-15");
    }

    #[test]
    fn grouping_handles_empty_input() {
        let groups: Vec<ActivityGroup> = group_activity(&[] as &[SseActivityEvent], 8);
        assert!(groups.is_empty());
    }
}
