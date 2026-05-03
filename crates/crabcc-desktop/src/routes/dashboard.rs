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
use gpui_component::{h_flex, v_flex, ActiveTheme};

use crate::api::types::{AgentStatus, SseActivityEvent};
use crate::routes::agent_spawn_sheet::AgentSpawnSheet;
use crate::routes::graph::GraphView;
use crate::state::AppState;

pub struct DashboardHome {
    state: Entity<AppState>,
    graph_view: Entity<GraphView>,
    /// Modal-ish launch sheet (#294 / A.9). Opened by the dashboard's
    /// "Launch agent…" CTA. The sheet self-closes on Detach / Kill /
    /// Open in Agents, so the host doesn't track its open state
    /// independently — render reads `spawn_sheet.is_open()` to decide
    /// whether to overlay it.
    spawn_sheet: Entity<AgentSpawnSheet>,
    /// Active op-pin on the Activity tile — set by clicking an op
    /// badge, cleared by clicking the active badge again or the
    /// header pin pill's `×`. Filters the activity buffer to that op
    /// before grouping. UI affordance per route, not on AppState
    /// (same call as the substring filters).
    activity_op_pin: Option<SharedString>,
    /// Reusable scratch buffer for `group_activity`. Cleared and
    /// refilled on every render — keeps the spine allocation across
    /// SSE-driven `notify()`s instead of allocating fresh each frame.
    activity_buffer: Vec<ActivityGroup>,
}

impl DashboardHome {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        let graph_view = cx.new(|cx| GraphView::new(state.clone(), cx));
        let spawn_sheet = cx.new(|cx| AgentSpawnSheet::new(state.clone(), window, cx));
        // Re-render the dashboard whenever the sheet's open/phase state
        // changes — the dashboard's own render decides whether to layer
        // the sheet element on top.
        cx.observe(&spawn_sheet, |_, _, cx| cx.notify()).detach();
        Self {
            state,
            graph_view,
            spawn_sheet,
            activity_op_pin: None,
            activity_buffer: Vec::with_capacity(8),
        }
    }

    /// Toggle activity op-pin. Clicking the active op clears it
    /// (saves the user hunting for the header `×` for casual
    /// narrow-then-clear).
    fn pin_activity_op(&mut self, op: SharedString) {
        if self.activity_op_pin.as_deref() == Some(op.as_ref()) {
            self.activity_op_pin = None;
        } else {
            self.activity_op_pin = Some(op);
        }
    }

    fn open_spawn_sheet(&self, cx: &mut Context<Self>) {
        self.spawn_sheet.update(cx, |sheet, cx| {
            sheet.open();
            cx.notify();
        });
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
        let agents_kpi = format!("{}/{} running", state.agents_running(), state.agents.len());
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
        // per family — see `op_color`. When `activity_op_pin` is set,
        // the buffer is pre-filtered to that op before grouping.
        let theme = cx.theme();
        let primary = theme.primary;
        let active_pin = self.activity_op_pin.clone();
        let activity_iter = state
            .recent_activity
            .iter()
            .filter(|e| match active_pin.as_deref() {
                None => true,
                Some(pinned) => e.op == pinned,
            });
        group_activity(activity_iter, 8, &mut self.activity_buffer);
        let groups_empty = self.activity_buffer.is_empty();
        // Use `last_event_ts` as a wall-clock proxy. Same trick as the
        // Agents-route relative-age formatter — keeps chrono out of
        // the dep tree for a tiny display tweak.
        let now_ts = state.last_event_ts.unwrap_or(0);
        let entity_for_op = cx.entity();
        let activity_body: gpui::AnyElement = if groups_empty && active_pin.is_some() {
            div()
                .text_color(muted)
                .child(SharedString::from(format!(
                    "no \u{201C}{}\u{201D} activity in buffer",
                    active_pin.as_deref().unwrap_or("")
                )))
                .into_any_element()
        } else {
            v_flex()
                .gap_1()
                .children(self.activity_buffer.drain(..).enumerate().map(|(idx, g)| {
                    let op_color = op_color(&g.op, theme);
                    // Recency fade — newer rows render full alpha,
                    // older rows fade toward the floor. Applied to
                    // both the op-badge and the query text so the
                    // whole row dims as one unit. Muted-side meta
                    // already lives at low contrast, so leave it.
                    let age = (now_ts - g.latest_ts).max(0);
                    let alpha = fade_alpha_for_age(age);
                    let faded_op = with_alpha(op_color, alpha);
                    let faded_fg = with_alpha(theme.foreground, alpha);
                    // Click-to-pin on the op badge. Active op
                    // renders with a primary-colour border so
                    // it's recognisable even when the badge
                    // colour itself is muted (e.g. `outline`).
                    // gpui requires stateful elements to declare
                    // an id; suffixing with the row index keeps
                    // it unique per render pass without an
                    // extra alloc per group.
                    let badge_id: gpui::ElementId =
                        SharedString::from(format!("activity-op-{idx}")).into();
                    let badge_pinned = active_pin.as_deref() == Some(g.op.as_str());
                    let badge_border = if badge_pinned {
                        primary
                    } else {
                        gpui::transparent_black()
                    };
                    let entity = entity_for_op.clone();
                    let click_op = g.op.clone();
                    h_flex()
                                .gap_2()
                                // Op badge — fixed-width column so the
                                // query text aligns across rows.
                                .child(
                                    div()
                                        .id(badge_id)
                                        .w(px(80.0))
                                        .px_1()
                                        .border_1()
                                        .border_color(badge_border)
                                        .rounded_md()
                                        .text_color(faded_op)
                                        .child(g.op.clone())
                                        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                            let op = click_op.clone();
                                            entity.update(cx, |this, cx| {
                                                this.pin_activity_op(op);
                                                cx.notify();
                                            });
                                        }),
                                )
                                .child(
                                    div()
                                        .text_color(faded_fg)
                                        .child(SharedString::from(truncate(&g.latest_query, 60))),
                                )
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
                }))
                .into_any_element()
        };
        // Header pin-pill — only renders when an op is pinned. Acts as
        // the canonical clear-affordance (clicking the pinned badge
        // also toggles, but the pill is the place a user looks when
        // narrowing feels stuck).
        let pin_pill: gpui::AnyElement = match active_pin.as_ref() {
            None => div().into_any_element(),
            Some(op) => {
                let entity_for_clear = cx.entity();
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .id("activity-op-pin-clear")
                            .px_2()
                            .py_0p5()
                            .border_1()
                            .border_color(primary)
                            .rounded_md()
                            .text_color(primary)
                            .child(SharedString::from(format!("{op} \u{00D7}")))
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                entity_for_clear.update(cx, |this, cx| {
                                    this.activity_op_pin = None;
                                    cx.notify();
                                });
                            }),
                    )
                    .into_any_element()
            }
        };
        let activity_tile = tile(
            "Recent activity",
            card,
            border,
            v_flex().gap_2().child(pin_pill).child(activity_body),
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
            v_flex()
                .gap_1()
                .children(state.agents.iter().take(8).map(|a| {
                    let dot = match a.status {
                        AgentStatus::Running => "●",
                        AgentStatus::Exited => "○",
                    };
                    let kill_btn: gpui::AnyElement = if matches!(a.status, AgentStatus::Running) {
                        let id_for_click = a.id.clone();
                        let state_for_click = agents_state.clone();
                        // Pre-computed at SSE-decode time — no
                        // per-render `format!()` alloc. See
                        // `AgentDerived` in `api/types.rs`.
                        let element_id: gpui::ElementId = a.derived.kill_id_home.clone().into();
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
                            // a.id is now SharedString — clone is a refcount
                            // bump, no allocation per render.
                            .child(a.id.clone())
                            .child(div().text_color(muted).child(
                                a.runtime.clone().unwrap_or_else(|| "—".into()),
                            ))
                            .child(div().flex_1())
                            .child(kill_btn)
                            .into_any_element()
                })),
        );

        // Hoist the Some/None match outside `.children()` so each arm
        // can call its own builder method (children-iter vs single
        // child) — drops the `Vec<AnyElement>` round-trip both arms
        // were paying for type unification.
        let services_body = v_flex().gap_1();
        let services_body = match state.services.as_ref() {
            Some(rep) => services_body.children(rep.services.iter().take(10).map(|s| {
                let mark = if s.reachable { "✓" } else { "✗" };
                h_flex()
                    .gap_2()
                    .child(SharedString::from(mark.to_string()))
                    .child(s.name.clone())
                    .child(
                        div()
                            .text_color(muted)
                            .child(SharedString::from(format!("{}ms", s.latency_ms))),
                    )
                    .into_any_element()
            })),
            None => services_body.child(
                div()
                    .text_color(muted)
                    .child(SharedString::new_static("loading…")),
            ),
        };
        let services_tile = tile("Services", card, border, services_body);

        let tile_row = h_flex()
            .gap_3()
            .px_5()
            .py_2()
            .child(activity_tile)
            .child(agents_tile)
            .child(services_tile);

        // ── Spawn-agent CTA ────────────────────────────────────────
        // The launch flow lives in `AgentSpawnSheet` now (#294). The
        // dashboard just owns a button that opens the sheet, plus a
        // status_line that surfaces the most recent server response so
        // failed launches don't disappear silently if the user has
        // already detached.
        let primary = cx.theme().primary;
        let success = cx.theme().success;
        let danger = cx.theme().danger;
        let view_entity = cx.entity();
        let launch_btn = div()
            .id("agent-launch-open-sheet")
            .px_3()
            .py_1()
            .border_1()
            .border_color(primary)
            .rounded_md()
            .text_color(primary)
            .child(SharedString::new_static("Launch agent…"))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                view_entity.update(cx, |this, cx| this.open_spawn_sheet(cx));
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
            .child(h_flex().gap_2().child(launch_btn))
            .child(status_line);

        let graph_row = div().px_5().py_2().child(self.graph_view.clone());

        // Wrap the route body in a `relative()` container so the spawn
        // sheet can overlay it via `.absolute()` without affecting the
        // dashboard's flex layout. The sheet element renders an empty
        // div when `is_open == false`, so this overlay is zero-cost
        // when the sheet is closed.
        let sheet_open = self.spawn_sheet.read(cx).is_open();
        let body = v_flex()
            .size_full()
            .child(kpi_strip)
            .child(tile_row)
            .child(spawn_row)
            .child(graph_row);

        let mut shell = div().relative().size_full().child(body);
        if sheet_open {
            shell = shell.child(self.spawn_sheet.clone());
        }
        shell
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
        .child(div().text_sm().child(SharedString::new_static(title)))
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
    op: SharedString,
    latest_query: SharedString,
    latest_results: u64,
    /// Timestamp of the freshest event in the run. Drives the
    /// recency-fade in the render path so newer rows render at full
    /// opacity and older ones fade toward muted.
    latest_ts: i64,
    count: usize,
}

/// Walk the buffer newest-first, collapsing runs of the same `op`
/// into a single group whose `count` carries the run length and
/// whose `latest_*` fields show the most-recent event in the run.
/// Fills `out` with up to `cap` groups.
///
/// Caller-owned `out` so the spine `Vec<ActivityGroup>` survives
/// across renders. Cleared on entry; re-filled in place. The inner
/// `SharedString` fields can still be `drain(..)`-ed by the caller
/// without losing the spine capacity.
fn group_activity<'a, I>(events: I, cap: usize, out: &mut Vec<ActivityGroup>)
where
    I: IntoIterator<Item = &'a SseActivityEvent>,
    I::IntoIter: DoubleEndedIterator,
{
    out.clear();
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
            latest_ts: evt.ts,
            count: 1,
        });
    }
}

/// Map a row's age (seconds since `now_ts`) to a multiplicative alpha
/// for the recency-fade. Rows fresher than [`FADE_FRESH_SECS`] render
/// at full opacity; rows older than [`FADE_STALE_SECS`] floor at
/// [`FADE_FLOOR_ALPHA`]; in-between fades linearly. Tuning rationale
/// in the constants' doc comments.
fn fade_alpha_for_age(age_secs: i64) -> f32 {
    if age_secs <= FADE_FRESH_SECS {
        return 1.0;
    }
    if age_secs >= FADE_STALE_SECS {
        return FADE_FLOOR_ALPHA;
    }
    let span = (FADE_STALE_SECS - FADE_FRESH_SECS) as f32;
    let into = (age_secs - FADE_FRESH_SECS) as f32;
    let t = (into / span).clamp(0.0, 1.0);
    1.0 - t * (1.0 - FADE_FLOOR_ALPHA)
}

/// Anything within this many seconds of `now` renders at full
/// opacity — short enough that activity in the last poll tick stays
/// crisp.
const FADE_FRESH_SECS: i64 = 5;
/// Above this many seconds, rows render at [`FADE_FLOOR_ALPHA`].
/// Tuned to match the activity-buffer churn rate — at typical work
/// pace the bottom of an 8-row buffer is ~30s old.
const FADE_STALE_SECS: i64 = 60;
/// Floor alpha for the oldest visible row. Kept above 0.5 so the
/// row stays legible — the fade is a "weight" cue, not "hide" cue.
const FADE_FLOOR_ALPHA: f32 = 0.55;

fn with_alpha(c: Hsla, a: f32) -> Hsla {
    Hsla { a, ..c }
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
        let mut groups = Vec::new();
        group_activity(&events, 8, &mut groups);
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
        let mut groups = Vec::new();
        group_activity(&events, 5, &mut groups);
        // Each event has a unique op, so groups equal events. We expect
        // exactly 5 (the cap) — the *newest* 5.
        assert_eq!(groups.len(), 5);
        assert_eq!(groups[0].op, "op-19");
        assert_eq!(groups[4].op, "op-15");
    }

    #[test]
    fn grouping_handles_empty_input() {
        let mut groups: Vec<ActivityGroup> = Vec::new();
        group_activity(&[] as &[SseActivityEvent], 8, &mut groups);
        assert!(groups.is_empty());
    }

    #[test]
    fn grouping_clears_existing_buffer_on_entry() {
        // Buffer-reuse contract: a fresh call clears prior contents
        // and refills in place — and the spine capacity survives so
        // the steady-state allocation count is zero.
        let mut groups = Vec::with_capacity(16);
        let first = vec![evt("sym", "x", 1)];
        group_activity(&first, 8, &mut groups);
        assert_eq!(groups.len(), 1);
        let cap_before = groups.capacity();

        let second = vec![evt("refs", "y", 2), evt("callers", "z", 3)];
        group_activity(&second, 8, &mut groups);
        // Old "sym" entry must be gone — buffer was cleared, not
        // appended.
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].op, "callers");
        assert_eq!(groups[1].op, "refs");
        // Spine capacity preserved (the whole point of the param flip).
        assert!(groups.capacity() >= cap_before);
    }
}
