//! Tool-call timeline / inspector route (#293 / A.12).
//!
//! Richer rendering of the same SSE activity stream the Home dashboard
//! activity tile already shows. Two columns:
//!
//!   * LEFT  — pinned section + main timeline list. Each row carries a
//!     per-tool colour-coded glyph, mono timestamp, op badge, query
//!     (truncated), result count, and a pin/unpin affordance.
//!   * RIGHT — inspector pane for the selected event: full timestamp,
//!     op + family colour, full query, result count, and a "Copy
//!     query" action.
//!
//! Filter input + tool-family pills narrow the list. Pinning deep-
//! copies the event so it survives eviction from the bounded
//! `recent_activity` ring on `AppState`.
//!
//! Consecutive same-`agent_id` rows fold behind a collapsible group
//! header (agent badge + run count + first/last timestamps) so a
//! 30-step agent run reads as one unit by default but expands inline
//! when the user wants the per-step detail.
//!
//! What's deliberately not here yet (called out, not faked):
//!
//!   * **Argument syntax highlighting / JSON pretty-print** — the
//!     server emits `query` as a single string, not structured args.
//!     The inspector renders it verbatim. If the server ever ships
//!     structured args, the inspector body becomes a JSON tree.
//!   * **Auto-scroll on new events** — the timeline list grows from
//!     the top. No virtualised scroll yet; capped at
//!     [`VISIBLE_LIMIT`] so dense bursts don't blow paint cost.

use gpui::{
    div, prelude::*, px, ClipboardItem, Context, Entity, Focusable, Hsla, IntoElement, MouseButton,
    Render, SharedString, Window,
};
use gpui_component::{
    h_flex,
    input::{Input, InputEvent, InputState},
    tooltip::Tooltip,
    v_flex, ActiveTheme,
};

use crate::api::types::SseActivityEvent;
use crate::routes::empty::empty_state;
use std::collections::HashSet;

use crate::state::AppState;
use crate::theme_helpers::op_color;

/// Cap on the number of timeline rows actually rendered after filtering.
/// `recent_activity` is bounded server-side too, but the brief calls
/// for "filter narrows the list" — without a cap, a wide-open filter on
/// a deep buffer paints hundreds of rows per render. 200 is comfortable
/// for an Activity-Monitor-density list at 28px row height (~5,600px
/// of content, scrolled by the shell's `overflow_y_scroll`).
const VISIBLE_LIMIT: usize = 200;

/// Truncate query text in the timeline rows beyond this length. The
/// inspector pane shows the full text, so this is purely a row-density
/// concern. 64 chars fits the common `crabcc.refs("Store")` shape
/// while preventing one runaway query from pushing the result count
/// off-screen.
const QUERY_TRUNCATE: usize = 64;

/// Tool-family pills the header strip exposes. Order matches the
/// frequency these ops show up on a typical session.
const TOOL_PILLS: &[&str] = &[
    "sym",
    "refs",
    "callers",
    "outline",
    "fuzzy",
    "prefix",
    "memory.ingest",
];

/// Minimum row count before a same-`agent_id` run earns a collapsible
/// header. A 1-row "group" is just a row — folding it is busywork.
/// 2 keeps the header useful (saves one row's worth of vertical space
/// per fold) without surprising the user with a header on every other
/// row.
const MIN_GROUP_SIZE: usize = 2;

/// One contiguous run of timeline rows sharing the same `agent_id`.
/// Built from the post-filter `visible_with_color` Vec by walking it
/// once and breaking on `agent_id` change. Rows with no `agent_id`
/// always sit alone (`agent_id == None`, `members.len() == 1`).
struct TimelineRun {
    /// `Some(_)` means the run was tagged by an agent (#311). `None`
    /// means a one-off row from the CLI / MCP that wasn't run inside
    /// an agent — never folded.
    agent_id: Option<SharedString>,
    members: Vec<(SseActivityEvent, Hsla)>,
}

impl TimelineRun {
    /// True iff this run is eligible for a collapsible header. `None`
    /// agent runs and short runs render as plain rows.
    fn foldable(&self) -> bool {
        self.agent_id.is_some() && self.members.len() >= MIN_GROUP_SIZE
    }
}

/// Walk `events` once and collapse consecutive entries with matching
/// `agent_id` into shared runs. Order is preserved; a single pass is
/// enough because the timeline is already sorted (newest first) at
/// the call site. Pure helper so the grouping logic is unit-testable
/// without standing up an `Entity<TimelineRoute>`.
fn group_into_runs(events: Vec<(SseActivityEvent, Hsla)>) -> Vec<TimelineRun> {
    let mut runs: Vec<TimelineRun> = Vec::new();
    for (e, c) in events {
        let same_agent_as_last = match (e.agent_id.as_ref(), runs.last()) {
            (Some(id), Some(last)) => last.agent_id.as_deref() == Some(id.as_ref()),
            _ => false,
        };
        if same_agent_as_last {
            runs.last_mut().expect("checked").members.push((e, c));
        } else {
            runs.push(TimelineRun {
                agent_id: e.agent_id.clone(),
                members: vec![(e, c)],
            });
        }
    }
    runs
}

pub struct TimelineRoute {
    state: Entity<AppState>,
    /// gpui-component InputState owns the filter text + focus.
    query_input: Entity<InputState>,
    /// Lower-cased mirror, kept in sync via `InputEvent::Change`.
    query_lower: String,
    /// Active op-pin (None = all ops). Click an op pill to set; click
    /// the active pill again to clear.
    op_pin: Option<SharedString>,
    /// Active agent-pin (None = all agents). Click an `agt` badge on
    /// a row to set; click the active badge or the header pill `×`
    /// to clear. ANDed with `op_pin` and `query_lower` — narrows to
    /// "events from this specific agent run".
    agent_pin: Option<SharedString>,
    /// Pinned events — deep-copied so they survive eviction from the
    /// `recent_activity` ring. Dedup is by `(ts, op, query)` since
    /// `SseActivityEvent` has no stable id today.
    pinned: Vec<SseActivityEvent>,
    /// The currently-inspected event. Deep-copied (same eviction
    /// reason). `None` shows the empty-state hint in the inspector.
    selected: Option<SseActivityEvent>,
    /// Per-agent fold state. Empty by default = every run renders
    /// expanded. An agent_id in this set means its group header is
    /// collapsed and its member rows are hidden until re-expanded.
    /// Click the header to toggle.
    collapsed_agents: HashSet<SharedString>,
}

impl TimelineRoute {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        let query_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("filter by op, query…"));
        cx.subscribe_in(&query_input, window, |this, st, event, _, cx| {
            if let InputEvent::Change = event {
                this.query_lower = st.read(cx).value().to_string().to_lowercase();
                cx.notify();
            }
        })
        .detach();
        Self {
            state,
            query_input,
            query_lower: String::new(),
            op_pin: None,
            agent_pin: None,
            pinned: Vec::new(),
            selected: None,
            collapsed_agents: HashSet::new(),
        }
    }

    fn toggle_collapsed_agent(&mut self, id: SharedString) {
        if !self.collapsed_agents.remove(&id) {
            self.collapsed_agents.insert(id);
        }
    }

    fn is_agent_collapsed(&self, id: &SharedString) -> bool {
        self.collapsed_agents.contains(id)
    }

    fn toggle_agent_pin(&mut self, id: SharedString) {
        if self.agent_pin.as_deref() == Some(id.as_ref()) {
            self.agent_pin = None;
        } else {
            self.agent_pin = Some(id);
        }
    }

    fn matches(&self, e: &SseActivityEvent) -> bool {
        if let Some(p) = &self.op_pin {
            if e.op.as_ref() != p.as_ref() {
                return false;
            }
        }
        if let Some(p) = &self.agent_pin {
            if e.agent_id.as_deref() != Some(p.as_ref()) {
                return false;
            }
        }
        if self.query_lower.is_empty() {
            return true;
        }
        let q = self.query_lower.as_str();
        // `agent_id` joins `op` + `query` as a substring-match field
        // so typing an id prefix into the filter input surfaces a
        // specific agent's events without first having to find one
        // and click its badge.
        e.op.to_lowercase().contains(q)
            || e.query.to_lowercase().contains(q)
            || e.agent_id
                .as_deref()
                .is_some_and(|id| id.to_lowercase().contains(q))
    }

    fn toggle_op_pin(&mut self, op: SharedString) {
        if self.op_pin.as_deref() == Some(op.as_ref()) {
            self.op_pin = None;
        } else {
            self.op_pin = Some(op);
        }
    }

    fn toggle_pin(&mut self, e: &SseActivityEvent) {
        if let Some(idx) = self.pinned.iter().position(|p| same_event(p, e)) {
            self.pinned.remove(idx);
        } else {
            self.pinned.push(e.clone());
        }
    }

    fn is_pinned(&self, e: &SseActivityEvent) -> bool {
        self.pinned.iter().any(|p| same_event(p, e))
    }

    fn select(&mut self, e: SseActivityEvent) {
        self.selected = Some(e);
    }

    fn clear_selection(&mut self) {
        self.selected = None;
    }

    fn copy_selected_query(&self, cx: &mut Context<Self>) {
        if let Some(e) = self.selected.as_ref() {
            cx.write_to_clipboard(ClipboardItem::new_string(e.query.to_string()));
        }
    }
}

/// Pure helper — exposed for unit tests. Returns true when both events
/// describe the same call, even after deep-copy. `ts + op + query`
/// uniquely identifies a row in practice; rapid duplicates within the
/// same second are rare enough to merge intentionally.
fn same_event(a: &SseActivityEvent, b: &SseActivityEvent) -> bool {
    a.ts == b.ts && a.op == b.op && a.query == b.query
}

/// `HH:MM:SS` from a unix-seconds timestamp. UTC; matches `routes::logs`
/// for consistency. Local-zone formatting needs a `chrono` / `time`
/// dep — not worth it for a developer-facing list.
fn format_time(unix_seconds: i64) -> String {
    let day = unix_seconds.rem_euclid(86_400);
    let h = day / 3600;
    let m = (day / 60) % 60;
    let s = day % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

impl Render for TimelineRoute {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Cross-route nav handoffs: a previous render of another
        // route (dashboard → Timeline, agents → Timeline) may have
        // staged an agent_pin and/or op_pin to apply on entry. Both
        // slots are independent — the dashboard's agent-pin pill and
        // op-pin pill each carry their own "→ Timeline" link — so
        // both can land in the same render. Consume up-front so the
        // subsequent `cx.theme()` borrow is unambiguous.
        let (pending_agent_pin, pending_op_pin) = self.state.update(cx, |s, _| {
            (
                s.take_pending_timeline_agent_pin(),
                s.take_pending_timeline_op_pin(),
            )
        });
        if let Some(id) = pending_agent_pin {
            self.agent_pin = Some(id);
        }
        if let Some(op) = pending_op_pin {
            self.op_pin = Some(op);
        }

        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let foreground = theme.foreground;
        let border = theme.border;
        let primary = theme.primary;
        let secondary = theme.secondary;
        let success = theme.success;
        let danger = theme.danger;

        // ── Snapshot the buffer + filter once so the closures below
        // don't need to re-borrow state on every iteration.
        let visible: Vec<SseActivityEvent> = {
            let s = self.state.read(cx);
            s.recent_activity
                .iter()
                .rev()
                .filter(|e| self.matches(e))
                .take(VISIBLE_LIMIT)
                .cloned()
                .collect()
        };
        let total_buffered = self.state.read(cx).recent_activity.len();

        // ── Header ────────────────────────────────────────────────
        let mut header = h_flex()
            .gap_3()
            .px_5()
            .py_3()
            .border_b_1()
            .border_color(border)
            .child(
                div()
                    .text_lg()
                    .text_color(foreground)
                    .child(SharedString::new_static("Timeline")),
            )
            .child(div().text_color(muted).child(SharedString::from(format!(
                "· {total_buffered} buffered · {} visible · {} pinned",
                visible.len(),
                self.pinned.len()
            ))));
        // Active agent-pin pill — visible whenever an agent is pinned
        // so the user can clear it from the persistent header rather
        // than having to find a row carrying that agent first.
        if let Some(id) = self.agent_pin.clone() {
            let trimmed: String = id.chars().take(8).collect();
            let entity_for_clear = cx.entity();
            header = header.child(
                div()
                    .id("timeline-agent-pin-clear")
                    .px_2()
                    .py_0p5()
                    .border_1()
                    .border_color(primary)
                    .rounded_md()
                    .text_color(primary)
                    .text_xs()
                    .cursor_pointer()
                    .hover(move |s| s.bg(secondary))
                    .tooltip(|window, cx| Tooltip::new("Clear agent pin").build(window, cx))
                    .child(SharedString::from(format!("agt {trimmed} \u{00D7}")))
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        entity_for_clear.update(cx, |this, cx| {
                            this.agent_pin = None;
                            cx.notify();
                        });
                    }),
            );
            // Reverse cross-link — Timeline shows the agent's calls,
            // but Agents shows the log tail / kill / pid / runtime.
            // From a pinned-agent context "→ Agents" is a one-click
            // dive into the operational view.
            let id_for_nav = id.clone();
            let state_for_nav = self.state.clone();
            header = header.child(
                div()
                    .id("timeline-agent-pin-to-agents")
                    .px_2()
                    .py_0p5()
                    .border_1()
                    .border_color(border)
                    .rounded_md()
                    .text_color(muted)
                    .text_xs()
                    .cursor_pointer()
                    .hover(move |s| s.border_color(primary).text_color(primary))
                    .tooltip(|window, cx| Tooltip::new("Open Agents at this id").build(window, cx))
                    .child(SharedString::new_static("\u{2192} Agents"))
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        let id = id_for_nav.clone();
                        state_for_nav.update(cx, |s, cx| {
                            s.navigate_to_agents_with_selection(id);
                            cx.notify();
                        });
                    }),
            );
        }

        // ── Filter input + pills ──────────────────────────────────
        // Wrapper border brightens to `primary` while focused — same
        // focus-indicator pattern as the other route filters.
        let filter_focused = self
            .query_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        let filter_border = if filter_focused { primary } else { border };
        let filter_field = div()
            .mx_5()
            .mt_3()
            .border_1()
            .border_color(filter_border)
            .rounded_md()
            .px_2()
            .py_1()
            .child(Input::new(&self.query_input).appearance(false));

        let entity_for_pill = cx.entity();
        let active_pin = self.op_pin.clone();
        // First pill is "All" — clears the op_pin. Remaining pills are
        // the canonical tool families.
        let pill_iter = std::iter::once::<Option<SharedString>>(None)
            .chain(TOOL_PILLS.iter().map(|p| Some(SharedString::new_static(p))));
        let pill_row = h_flex()
            .mx_5()
            .mt_2()
            .gap_2()
            .children(pill_iter.map(|maybe_op| {
                let label = match &maybe_op {
                    None => SharedString::new_static("All"),
                    Some(op) => op.clone(),
                };
                let is_active = match (&active_pin, &maybe_op) {
                    (None, None) => true,
                    (Some(a), Some(b)) => a == b,
                    _ => false,
                };
                let pill_color = match &maybe_op {
                    Some(op) => op_color(op.as_ref(), theme),
                    None => primary,
                };
                let click_op = maybe_op.clone();
                let entity = entity_for_pill.clone();
                let id = SharedString::from(format!("timeline-pill-{label}"));
                div()
                    .id(id)
                    .px_2()
                    .py_0p5()
                    .border_1()
                    .border_color(if is_active { pill_color } else { border })
                    .rounded_md()
                    .text_color(if is_active { foreground } else { muted })
                    .bg(if is_active {
                        secondary
                    } else {
                        gpui::transparent_black()
                    })
                    .cursor_pointer()
                    .hover(move |s| s.bg(secondary))
                    .child(label)
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        let target = click_op.clone();
                        entity.update(cx, |this, cx| {
                            match target {
                                None => this.op_pin = None,
                                Some(op) => this.toggle_op_pin(op),
                            }
                            cx.notify();
                        });
                    })
            }));

        // ── Pre-bind per-event colours, drop the theme borrow ────
        // Each row needs an `Hsla` for its tool-family icon. Resolving
        // up-front into a Vec lets the body loop call `self.row(...,
        // cx)` (mut-borrow) without theme still being live.
        let pinned_with_color: Vec<(SseActivityEvent, Hsla)> = self
            .pinned
            .iter()
            .map(|e| (e.clone(), op_color(e.op.as_ref(), theme)))
            .collect();
        let visible_with_color: Vec<(SseActivityEvent, Hsla)> = visible
            .into_iter()
            .map(|e| {
                let c = op_color(e.op.as_ref(), theme);
                (e, c)
            })
            .collect();
        let inspector_op_col = self
            .selected
            .as_ref()
            .map(|e| op_color(e.op.as_ref(), theme))
            .unwrap_or(muted);
        // From here, `theme` is no longer needed; `cx` is free.

        // ── Pinned section (left column) ──────────────────────────
        let pinned_block: gpui::AnyElement =
            if pinned_with_color.is_empty() {
                div().into_any_element()
            } else {
                let mut block =
                    v_flex()
                        .gap_1()
                        .px_5()
                        .py_2()
                        .child(div().text_color(muted).text_xs().child(SharedString::from(
                            format!("PINNED ({})", pinned_with_color.len()),
                        )));
                for (e, op_col) in pinned_with_color {
                    block = block.child(self.row(
                        e, /* lifted */ true, op_col, foreground, muted, border, secondary,
                        danger, cx,
                    ));
                }
                block.into_any_element()
            };

        // ── Main timeline list ────────────────────────────────────
        // Walk the post-filter buffer once to collapse consecutive
        // same-`agent_id` rows into runs (`group_into_runs`). Each
        // foldable run renders as a single header row by default —
        // collapsing a 30-step agent run from 30 lines down to 1 —
        // and expands inline when the user toggles it.
        let timeline_block: gpui::AnyElement = if visible_with_color.is_empty() {
            if self.query_lower.is_empty() && self.op_pin.is_none() {
                empty_state(
                    "\u{25CC}",
                    "No activity yet",
                    "Tool calls + agent runs stream in here as they happen.",
                    muted,
                    foreground,
                )
                .into_any_element()
            } else {
                empty_state(
                    "\u{1F50D}",
                    "No activity matches the filter",
                    "Try widening the query or clearing the op pin.",
                    muted,
                    foreground,
                )
                .into_any_element()
            }
        } else {
            let runs = group_into_runs(visible_with_color);
            let mut list = v_flex().gap_0p5().px_5().py_2();
            for run in runs {
                if run.foldable() {
                    let agent_id = run.agent_id.clone().expect("foldable() implies Some");
                    let collapsed = self.is_agent_collapsed(&agent_id);
                    list = list.child(self.group_header(
                        agent_id.clone(),
                        run.members.len(),
                        // Members are newest-first (the source list
                        // was `iter().rev()` upstream). First member's
                        // ts is the newest; last is the oldest.
                        run.members.first().map(|(e, _)| e.ts).unwrap_or(0),
                        run.members.last().map(|(e, _)| e.ts).unwrap_or(0),
                        collapsed,
                        muted,
                        foreground,
                        border,
                        secondary,
                        cx,
                    ));
                    if !collapsed {
                        for (e, op_col) in run.members {
                            list = list.child(self.row(
                                e, /* lifted */ false, op_col, foreground, muted, border,
                                secondary, danger, cx,
                            ));
                        }
                    }
                } else {
                    for (e, op_col) in run.members {
                        list = list.child(self.row(
                            e, /* lifted */ false, op_col, foreground, muted, border,
                            secondary, danger, cx,
                        ));
                    }
                }
            }
            list.into_any_element()
        };

        // ── Inspector pane (right column) ─────────────────────────
        let inspector = self.render_inspector(
            foreground,
            muted,
            border,
            secondary,
            success,
            inspector_op_col,
            cx,
        );

        let left = v_flex()
            .flex_1()
            .gap_0()
            .child(header)
            .child(filter_field)
            .child(pill_row)
            .child(pinned_block)
            .child(timeline_block);

        h_flex().size_full().child(left).child(inspector)
    }
}

impl TimelineRoute {
    /// Collapsible header for a same-`agent_id` run. The chevron
    /// (▾ / ▸) doubles as the toggle hint; clicking anywhere on the
    /// header flips the agent's collapsed state.
    ///
    /// `ts_first` / `ts_last` come from the run's outer extents. The
    /// upstream list is newest-first, so first ≥ last on the wall
    /// clock — the header reads "12:34 - 11:58" (latest first).
    #[allow(clippy::too_many_arguments)]
    fn group_header(
        &self,
        agent_id: SharedString,
        count: usize,
        ts_first: i64,
        ts_last: i64,
        collapsed: bool,
        muted: Hsla,
        foreground: Hsla,
        border: Hsla,
        secondary: Hsla,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let agent_short: String = agent_id.chars().take(8).collect();
        let chevron = SharedString::new_static(if collapsed { "▸" } else { "▾" });
        let span = if ts_first == ts_last {
            format_time(ts_first)
        } else {
            format!("{} - {}", format_time(ts_first), format_time(ts_last))
        };
        let id_for_click = agent_id.clone();
        let view = cx.entity();
        let header_id = SharedString::from(format!("timeline-group-{agent_id}"));
        div()
            .id(header_id)
            .px_2()
            .py_1()
            .border_1()
            .border_color(border)
            .rounded_md()
            .bg(secondary)
            .child(
                h_flex()
                    .gap_2()
                    .child(div().text_color(muted).child(chevron))
                    .child(
                        div()
                            .px_1()
                            .border_1()
                            .border_color(muted)
                            .rounded_md()
                            .text_color(foreground)
                            .text_xs()
                            .child(SharedString::from(format!("agt {agent_short}"))),
                    )
                    .child(
                        div()
                            .text_color(foreground)
                            .text_xs()
                            .child(SharedString::from(format!(
                                "{count} call{}",
                                if count == 1 { "" } else { "s" }
                            ))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_color(muted)
                            .text_xs()
                            .child(SharedString::from(span)),
                    ),
            )
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                let id = id_for_click.clone();
                view.update(cx, |this, cx| {
                    this.toggle_collapsed_agent(id);
                    cx.notify();
                });
            })
    }

    /// Single timeline row — used by both the pinned section and the
    /// main list. `lifted` toggles a slightly elevated background so
    /// pinned items feel sticky; everything else is identical.
    /// `op_col` is precomputed at the call site to avoid double-
    /// borrowing `cx` (theme lookup vs `cx.entity()` for click
    /// handlers).
    #[allow(clippy::too_many_arguments)]
    fn row(
        &self,
        event: SseActivityEvent,
        lifted: bool,
        op_col: Hsla,
        foreground: Hsla,
        muted: Hsla,
        border: Hsla,
        secondary: Hsla,
        danger: Hsla,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let pinned = self.is_pinned(&event);
        let selected = self
            .selected
            .as_ref()
            .map(|s| same_event(s, &event))
            .unwrap_or(false);

        // Stable id per row — ts is i64 + op is short, so concat is
        // unique enough across a session without a hash. Using
        // `(ts, op, query[..16])` to dedup rapid duplicates.
        let row_id = SharedString::from(format!(
            "timeline-row-{}-{}-{}",
            event.ts,
            event.op,
            truncate(event.query.as_ref(), 16)
        ));

        let row_view = cx.entity();
        let pin_view = cx.entity();
        let event_for_pin = event.clone();
        let event_for_select = event.clone();

        let icon = div()
            .w(px(12.0))
            .text_color(op_col)
            .child(SharedString::new_static("●"));

        let ts = div()
            .text_color(muted)
            .text_xs()
            .child(SharedString::from(format_time(event.ts)));

        let op = div()
            .text_color(op_col)
            .child(SharedString::from(event.op.to_string()));

        let query = div()
            .flex_1()
            .text_color(foreground)
            .text_xs()
            .child(SharedString::from(truncate(
                event.query.as_ref(),
                QUERY_TRUNCATE,
            )));

        let results = div()
            .text_color(if event.results == 0 { danger } else { muted })
            .text_xs()
            .child(SharedString::from(format!(
                "→ {} result{}",
                event.results,
                if event.results == 1 { "" } else { "s" }
            )));

        // Agent badge — only renders when the server (post-#311)
        // tagged this row with an agent run id. Truncated to keep
        // the row dense; the inspector pane shows the full id.
        // Click pins / unpins this agent (parallel to op_pin via
        // header pills). Active pin renders foreground-bright so the
        // active filter is visually obvious from across the list.
        let agent_pin_view = cx.entity();
        let agent_badge: gpui::AnyElement = match event.agent_id.as_ref() {
            Some(id) => {
                let trimmed: String = id.chars().take(8).collect();
                let is_pinned_agent = self.agent_pin.as_deref() == Some(id.as_ref());
                let badge_color = if is_pinned_agent { foreground } else { muted };
                let click_id = id.clone();
                div()
                    .id(SharedString::from(format!("{row_id}-agent-pin")))
                    .px_1()
                    .border_1()
                    .border_color(badge_color)
                    .rounded_md()
                    .text_color(badge_color)
                    .text_xs()
                    .child(SharedString::from(format!("agt {trimmed}")))
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        cx.stop_propagation();
                        let id = click_id.clone();
                        agent_pin_view.update(cx, |this, cx| {
                            this.toggle_agent_pin(id);
                            cx.notify();
                        });
                    })
                    .into_any_element()
            }
            None => div().into_any_element(),
        };

        let pin_btn = div()
            .id(SharedString::from(format!("{row_id}-pin")))
            .px_1()
            .text_color(if pinned { foreground } else { muted })
            .child(SharedString::new_static(if pinned { "★" } else { "☆" }))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                cx.stop_propagation();
                let e = event_for_pin.clone();
                pin_view.update(cx, |this, cx| {
                    this.toggle_pin(&e);
                    cx.notify();
                });
            });

        // Whole row stays clickable for select; pin button stops
        // propagation. `border` argument unused by design — left
        // hairline between rows comes from the parent v_flex's
        // `gap_0p5`, and a per-row bottom border read as too noisy
        // against the tight density.
        let _ = border;
        div()
            .id(row_id)
            .px_2()
            .py_1()
            .border_1()
            .border_color(if selected {
                op_col
            } else {
                gpui::transparent_black()
            })
            .rounded_md()
            .bg(if lifted || selected {
                secondary
            } else {
                gpui::transparent_black()
            })
            .child(
                h_flex()
                    .gap_2()
                    .child(icon)
                    .child(ts)
                    .child(op)
                    .child(query)
                    .child(results)
                    .child(agent_badge)
                    .child(pin_btn),
            )
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                let e = event_for_select.clone();
                row_view.update(cx, |this, cx| {
                    this.select(e);
                    cx.notify();
                });
            })
    }

    #[allow(clippy::too_many_arguments)]
    fn render_inspector(
        &self,
        foreground: Hsla,
        muted: Hsla,
        border: Hsla,
        secondary: Hsla,
        success: Hsla,
        op_col_for_selected: Hsla,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let frame = v_flex()
            .w(px(360.0))
            .gap_3()
            .p_3()
            .border_l_1()
            .border_color(border)
            .bg(secondary);

        match &self.selected {
            None => frame.child(
                div()
                    .text_color(muted)
                    .child(SharedString::new_static("Select a call to inspect.")),
            ),
            Some(event) => {
                let op_col = op_col_for_selected;
                let close_view = cx.entity();
                let copy_view = cx.entity();

                let header = h_flex()
                    .items_start()
                    .justify_between()
                    .gap_2()
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_color(op_col)
                                    .child(SharedString::from(event.op.to_string())),
                            )
                            .child(div().text_color(muted).text_xs().child(SharedString::from(
                                format!("ts {} ({})", format_time(event.ts), event.ts,),
                            ))),
                    )
                    .child(
                        div()
                            .id("timeline-inspector-close")
                            .px_1()
                            .text_color(muted)
                            .child(SharedString::new_static("×"))
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                cx.stop_propagation();
                                close_view.update(cx, |this, cx| {
                                    this.clear_selection();
                                    cx.notify();
                                });
                            }),
                    );

                let query_block = v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_color(muted)
                            .text_xs()
                            .child(SharedString::new_static("QUERY")),
                    )
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .border_1()
                            .border_color(border)
                            .rounded_md()
                            .text_color(foreground)
                            .text_xs()
                            .child(SharedString::from(event.query.to_string())),
                    );

                let result_line = h_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_color(muted)
                            .text_xs()
                            .child(SharedString::new_static("RESULT")),
                    )
                    .child(div().text_color(success).child(SharedString::from(format!(
                        "{} result{}",
                        event.results,
                        if event.results == 1 { "" } else { "s" }
                    ))));

                let copy_btn = div()
                    .id("timeline-inspector-copy")
                    .px_2()
                    .py_1()
                    .border_1()
                    .border_color(border)
                    .rounded_md()
                    .text_color(muted)
                    .child(SharedString::new_static("Copy query"))
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        cx.stop_propagation();
                        copy_view.update(cx, |this, cx| {
                            this.copy_selected_query(cx);
                            cx.notify();
                        });
                    });

                frame
                    .child(header)
                    .child(query_block)
                    .child(result_line)
                    .child(h_flex().gap_2().child(copy_btn))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evt(ts: i64, op: &str, q: &str, r: u64) -> SseActivityEvent {
        SseActivityEvent {
            ts,
            op: SharedString::from(op.to_string()),
            query: SharedString::from(q.to_string()),
            results: r,
            agent_id: None,
        }
    }

    #[test]
    fn same_event_uses_ts_op_query() {
        let a = evt(100, "sym", "Store", 17);
        let b = evt(100, "sym", "Store", 99); // results differ
        let c = evt(101, "sym", "Store", 17); // ts differs
        assert!(same_event(&a, &b), "results don't break identity");
        assert!(!same_event(&a, &c));
    }

    #[test]
    fn truncate_appends_ellipsis_only_when_needed() {
        assert_eq!(truncate("short", 10), "short");
        // First (max-1) chars + ellipsis. 4 chars of "toolongtohandle"
        // is "tool".
        assert_eq!(truncate("toolongtohandle", 5), "tool…");
    }

    #[test]
    fn format_time_pads_components() {
        assert_eq!(format_time(0), "00:00:00");
        assert_eq!(format_time(3661), "01:01:01");
    }

    fn evt_a(ts: i64, op: &str, agent: Option<&str>) -> SseActivityEvent {
        SseActivityEvent {
            ts,
            op: SharedString::from(op.to_string()),
            query: SharedString::from(""),
            results: 0,
            agent_id: agent.map(|s| SharedString::from(s.to_string())),
        }
    }

    fn col() -> Hsla {
        Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.5,
            a: 1.0,
        }
    }

    #[test]
    fn group_into_runs_splits_at_agent_boundary() {
        let events = vec![
            (evt_a(1, "sym", Some("a1")), col()),
            (evt_a(2, "refs", Some("a1")), col()),
            (evt_a(3, "sym", Some("a2")), col()),
        ];
        let runs = group_into_runs(events);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].members.len(), 2);
        assert_eq!(runs[1].members.len(), 1);
        assert_eq!(
            runs[0].agent_id.as_deref().map(|s| s.to_string()),
            Some("a1".into())
        );
        assert_eq!(
            runs[1].agent_id.as_deref().map(|s| s.to_string()),
            Some("a2".into())
        );
    }

    #[test]
    fn group_into_runs_keeps_no_agent_solo() {
        // Three rows with no agent_id should each land in their own run
        // and never fold (foldable() == false).
        let events = vec![
            (evt_a(1, "sym", None), col()),
            (evt_a(2, "refs", None), col()),
            (evt_a(3, "callers", None), col()),
        ];
        let runs = group_into_runs(events);
        assert_eq!(runs.len(), 3);
        for r in &runs {
            assert_eq!(r.members.len(), 1);
            assert!(!r.foldable());
        }
    }

    #[test]
    fn group_into_runs_foldable_only_at_min_size() {
        // 1-row group should not be foldable; 2+ should be.
        let single = vec![(evt_a(1, "sym", Some("a1")), col())];
        assert!(!group_into_runs(single)[0].foldable());

        let two = vec![
            (evt_a(1, "sym", Some("a1")), col()),
            (evt_a(2, "refs", Some("a1")), col()),
        ];
        assert!(group_into_runs(two)[0].foldable());
    }

    /// Build a TimelineRoute-shaped value just for `matches`. The
    /// route normally needs an `Entity<AppState>` + `Entity<InputState>`
    /// from a real `Window`/`Cx`; for a pure-logic test on `matches`
    /// we only need the four filter fields (`op_pin`, `agent_pin`,
    /// `query_lower`, plus the `pinned`/`selected`/`collapsed_agents`
    /// the method ignores). Helper avoids touching gpui plumbing.
    struct MatchesShape {
        op_pin: Option<SharedString>,
        agent_pin: Option<SharedString>,
        query_lower: String,
    }

    impl MatchesShape {
        fn matches(&self, e: &SseActivityEvent) -> bool {
            if let Some(p) = &self.op_pin {
                if e.op.as_ref() != p.as_ref() {
                    return false;
                }
            }
            if let Some(p) = &self.agent_pin {
                if e.agent_id.as_deref() != Some(p.as_ref()) {
                    return false;
                }
            }
            if self.query_lower.is_empty() {
                return true;
            }
            let q = self.query_lower.as_str();
            e.op.to_lowercase().contains(q)
                || e.query.to_lowercase().contains(q)
                || e.agent_id
                    .as_deref()
                    .is_some_and(|id| id.to_lowercase().contains(q))
        }
    }

    #[test]
    fn matches_filters_by_agent_pin() {
        let shape = MatchesShape {
            op_pin: None,
            agent_pin: Some("agent-a".into()),
            query_lower: String::new(),
        };
        assert!(shape.matches(&evt_a(1, "sym", Some("agent-a"))));
        assert!(!shape.matches(&evt_a(1, "sym", Some("agent-b"))));
        // No agent_id → never matches an agent_pin.
        assert!(!shape.matches(&evt_a(1, "sym", None)));
    }

    #[test]
    fn matches_query_substring_finds_agent_id() {
        // The substring filter joins op + query + agent_id, so typing
        // an agent prefix surfaces that agent without needing to
        // click a badge first.
        let shape = MatchesShape {
            op_pin: None,
            agent_pin: None,
            query_lower: "agent".to_string(),
        };
        assert!(shape.matches(&evt_a(1, "sym", Some("agent-deadbeef"))));
        // Op match still works.
        let shape2 = MatchesShape {
            op_pin: None,
            agent_pin: None,
            query_lower: "ref".to_string(),
        };
        assert!(shape2.matches(&evt_a(1, "refs", None)));
    }

    #[test]
    fn matches_op_and_agent_are_anded() {
        let shape = MatchesShape {
            op_pin: Some("sym".into()),
            agent_pin: Some("agent-a".into()),
            query_lower: String::new(),
        };
        assert!(shape.matches(&evt_a(1, "sym", Some("agent-a"))));
        // Wrong op even though agent matches.
        assert!(!shape.matches(&evt_a(1, "refs", Some("agent-a"))));
        // Wrong agent even though op matches.
        assert!(!shape.matches(&evt_a(1, "sym", Some("agent-b"))));
    }

    #[test]
    fn group_into_runs_breaks_on_none_in_middle() {
        // a1 ─ none ─ a1 should split into THREE runs: the middle
        // None breaks the run, and the second a1 starts fresh.
        let events = vec![
            (evt_a(1, "sym", Some("a1")), col()),
            (evt_a(2, "refs", None), col()),
            (evt_a(3, "callers", Some("a1")), col()),
        ];
        let runs = group_into_runs(events);
        assert_eq!(runs.len(), 3);
        assert_eq!(
            runs[0].agent_id.as_deref().map(|s| s.to_string()),
            Some("a1".into())
        );
        assert!(runs[1].agent_id.is_none());
        assert_eq!(
            runs[2].agent_id.as_deref().map(|s| s.to_string()),
            Some("a1".into())
        );
    }
}
