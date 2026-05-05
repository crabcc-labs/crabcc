//! Agents route — full-detail live agents list with substring filter +
//! click-to-tail log.
//!
//! The Home dashboard renders a compact 8-row tile with agent id /
//! runtime — fine at a glance, but it loses the model, pid, prompt
//! preview, log volume, and project root. This route lifts the same
//! `AppState::agents` slice into a dedicated page with all the SSE
//! fields visible and no row cap.
//!
//! Top-of-route TextInput filters the visible list as the user types
//! (id / runtime / model / prompt-preview, case-insensitive). The
//! filter lives on the route entity, not `AppState` — the filter is a
//! UI affordance that doesn't need to survive nav switches today.
//!
//! Clicking an agent card selects it and dispatches a one-shot
//! `GET /api/agents/{id}/log?since=0` via `AppState::submit_agent_log`.
//! The expanded card shows the last ~4 KiB of the body. A "Refresh"
//! affordance re-fires the same fetch; clicking the same card again
//! collapses the panel.

use gpui::{
    div, prelude::*, px, Context, Entity, Focusable, IntoElement, MouseButton, Render,
    SharedString, Window,
};
use gpui_component::{
    h_flex,
    input::{Input, InputEvent, InputState},
    tooltip::Tooltip,
    v_flex, ActiveTheme,
};

use crate::api::types::{AgentStatus, SseAgent};
use crate::routes::empty::empty_state;
use crate::state::AppState;

/// How many trailing bytes of the agent log to render. The server
/// caps the body at its own window already; this is a *display* cap
/// so the expanded panel doesn't grow unbounded if the user picks a
/// chatty agent.
const LOG_TAIL_BYTES: usize = 4096;

/// Status filter pill state. `None` means show all; `Some` narrows
/// to one status. Kept on the route entity (not `AppState`) — it's a
/// UI affordance, same call as the substring filter.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StatusFilter {
    All,
    Running,
    Exited,
}

impl StatusFilter {
    const ALL: [StatusFilter; 3] = [
        StatusFilter::All,
        StatusFilter::Running,
        StatusFilter::Exited,
    ];

    fn label(self) -> &'static str {
        match self {
            StatusFilter::All => "All",
            StatusFilter::Running => "Running",
            StatusFilter::Exited => "Exited",
        }
    }

    fn id(self) -> &'static str {
        match self {
            StatusFilter::All => "agents-pill-all",
            StatusFilter::Running => "agents-pill-running",
            StatusFilter::Exited => "agents-pill-exited",
        }
    }

    fn matches(self, a: &SseAgent) -> bool {
        match self {
            StatusFilter::All => true,
            StatusFilter::Running => a.status == AgentStatus::Running,
            StatusFilter::Exited => a.status == AgentStatus::Exited,
        }
    }
}

pub struct AgentsRoute {
    state: Entity<AppState>,
    /// gpui-component InputState — owns text + focus for the filter.
    query_input: Entity<InputState>,
    /// Lower-cased mirror of the input's value, kept in sync via
    /// `InputEvent::Change`. Avoids re-lowercasing the query for
    /// every match check on every render.
    query_lower: String,
    /// Currently selected agent id; `None` means no card is expanded.
    /// Click on the same id collapses; click on a new id selects +
    /// fires a fresh log fetch.
    selected_id: Option<SharedString>,
    /// Status pill filter, ANDed with `query_lower` for visibility.
    status_filter: StatusFilter,
}

impl AgentsRoute {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        let query_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter by id / runtime / model…"));
        cx.subscribe_in(&query_input, window, |this, state, event, _, cx| {
            if let InputEvent::Change = event {
                this.query_lower = state.read(cx).value().to_string().to_lowercase();
                cx.notify();
            }
        })
        .detach();
        Self {
            state,
            query_input,
            query_lower: String::new(),
            selected_id: None,
            status_filter: StatusFilter::All,
        }
    }

    fn agent_matches(&self, a: &SseAgent) -> bool {
        if !self.status_filter.matches(a) {
            return false;
        }
        if self.query_lower.is_empty() {
            return true;
        }
        let q = self.query_lower.as_str();
        if a.id.to_lowercase().contains(q) {
            return true;
        }
        if let Some(r) = a.runtime.as_ref() {
            if r.to_lowercase().contains(q) {
                return true;
            }
        }
        if let Some(m) = a.model.as_ref() {
            if m.to_lowercase().contains(q) {
                return true;
            }
        }
        a.prompt_preview.to_lowercase().contains(q)
    }

    /// Click handler — toggles selection and fires a fresh log fetch
    /// on a new selection. Re-fetch on the same id is a separate path
    /// (the "Refresh" affordance), so this stays single-shot per
    /// click.
    fn select_agent(&mut self, id: SharedString, cx: &mut Context<Self>) {
        if self.selected_id.as_deref() == Some(id.as_ref()) {
            // Toggle off.
            self.selected_id = None;
            return;
        }
        self.selected_id = Some(id.clone());
        self.state.read(cx).submit_agent_log(id, 0);
    }

    /// Re-fetch the log for the current selection.
    fn refresh_log(&self, cx: &mut Context<Self>) {
        let Some(id) = self.selected_id.clone() else {
            return;
        };
        self.state.read(cx).submit_agent_log(id, 0);
    }
}

impl Render for AgentsRoute {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Cross-route nav handoff: a previous render of another route
        // (e.g. Timeline → Agents via the agent-pin pill's link) may
        // have staged an id to pre-select. Consume up-front so the
        // log-tail fetch fires from the same render path as a manual
        // click. One-shot — subsequent renders don't fight a manual
        // deselect.
        let pending_select = self
            .state
            .update(cx, |s, _| s.take_pending_agents_selected_id());
        if let Some(id) = pending_select {
            // Skip the "click again to toggle off" branch in
            // `select_agent` by ensuring we're not already on this id.
            if self.selected_id.as_deref() != Some(id.as_ref()) {
                self.select_agent(id, cx);
            }
        }

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
        // Apply the filter once; we need the visible count for the
        // header and the iterator for the body, so collect into a Vec
        // rather than filter twice.
        let visible: Vec<&SseAgent> = state
            .agents
            .iter()
            .filter(|a| self.agent_matches(a))
            .collect();
        let visible_count = visible.len();

        // ── Header ──────────────────────────────────────────────────
        let count_label = if self.query_lower.is_empty() {
            format!("· {total} total · {running} running")
        } else {
            format!("· {visible_count} of {total} match")
        };
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
            .child(
                div()
                    .text_color(muted)
                    .child(SharedString::from(count_label)),
            );

        // ── Filter input ────────────────────────────────────────────
        // Brighten the wrapper border to `primary` when the inner
        // `InputState` has the window's focus — gives the user a
        // visible "you're typing here" cue. The wrapper itself is a
        // plain div, so we read the inner focus handle and switch
        // the border colour ourselves.
        let filter_focused = self
            .query_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        let filter_border = if filter_focused { primary } else { border };
        let search_field = div()
            .mx_5()
            .mt_3()
            .border_1()
            .border_color(filter_border)
            .rounded_md()
            .px_2()
            .py_1()
            .child(Input::new(&self.query_input).appearance(false));

        // ── Status pills ────────────────────────────────────────────
        let active_filter = self.status_filter;
        let entity_for_pill = cx.entity();
        let pill_row = h_flex()
            .mx_5()
            .mt_2()
            .gap_2()
            .children(StatusFilter::ALL.into_iter().map(|f| {
                let is_active = f == active_filter;
                let entity = entity_for_pill.clone();
                div()
                    .id(SharedString::new_static(f.id()))
                    .px_2()
                    .py_0p5()
                    .border_1()
                    .border_color(if is_active { primary } else { border })
                    .rounded_md()
                    .text_color(if is_active { foreground } else { muted })
                    .cursor_pointer()
                    .hover(move |s| s.border_color(primary))
                    .child(SharedString::new_static(f.label()))
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        entity.update(cx, |this, cx| {
                            this.status_filter = f;
                            cx.notify();
                        });
                    })
                    .into_any_element()
            }));

        // ── Body ────────────────────────────────────────────────────
        let body: gpui::AnyElement = if state.agents.is_empty() {
            empty_state(
                "\u{25CC}",
                "No agents tracked yet",
                "Launch one from Home, or run `crabcc agents` from the CLI.",
                muted,
                cx.theme().foreground,
            )
            .into_any_element()
        } else if visible.is_empty() {
            empty_state(
                "\u{1F50D}",
                "No agents match the filter",
                &format!(
                    "Nothing matches \u{201C}{}\u{201D} — try a shorter query.",
                    self.query_lower
                ),
                muted,
                cx.theme().foreground,
            )
            .into_any_element()
        } else {
            let selected_id = self.selected_id.clone();
            let agent_log = state.agent_log.as_ref();
            let entity_for_click = cx.entity();
            let entity_for_refresh = cx.entity();
            let danger = cx.theme().danger;
            let agents_state = self.state.clone();

            v_flex()
                .px_5()
                .py_2()
                .gap_2()
                .children(visible.into_iter().map(|a| {
                    let is_selected = selected_id.as_deref() == Some(a.id.as_str());
                    let dot = match a.status {
                        AgentStatus::Running => "●",
                        AgentStatus::Exited => "○",
                    };
                    let dot_color = match a.status {
                        AgentStatus::Running => success,
                        AgentStatus::Exited => muted,
                    };
                    let runtime = a
                        .runtime
                        .clone()
                        .unwrap_or_else(|| "subprocess (host)".into());
                    let model = a.model.clone(); // Option — we hide the chip when None.
                    let pid = a.pid; // Option — hidden in the row when None.
                                     // Best-effort start-time formatter. We avoid pulling
                                     // chrono just for this — `started_ts` is unix-seconds,
                                     // and "Xs ago" is what a glance-pane wants anyway.
                    let age = relative_age(a.started_ts, state.last_event_ts);
                    let log_label = format_log_size(a.log_bytes);
                    // Drop the absolute path; show the leaf so the meta
                    // row stays readable. None → omit the chip rather
                    // than render "root: —".
                    let root_short: Option<SharedString> = a
                        .root
                        .as_ref()
                        .and_then(|r| r.rsplit('/').next())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string().into());

                    // Kill button — running agents only. Mirrors the
                    // Home Agents-tile pattern (#234) but lifted into
                    // this route so a user filtering / drilling in
                    // here can also stop a misbehaving agent without
                    // navigating back. Click stops propagation so it
                    // doesn't bubble into the outer card click that
                    // would expand / collapse the log panel.
                    let kill_btn: gpui::AnyElement = if matches!(a.status, AgentStatus::Running) {
                        let id_for_kill = a.id.clone();
                        let id_for_tooltip: SharedString =
                            a.id.chars().take(8).collect::<String>().into();
                        let state_for_kill = agents_state.clone();
                        // Pre-computed at SSE-decode time — no
                        // per-render `format!()` alloc. See
                        // `AgentDerived` in `api/types.rs`.
                        let element_id: gpui::ElementId =
                            a.derived.kill_id_agents_route.clone().into();
                        div()
                            .id(element_id)
                            .px_2()
                            .py_0p5()
                            .border_1()
                            .border_color(danger)
                            .rounded_md()
                            .text_color(danger)
                            .cursor_pointer()
                            .hover(move |s| s.bg(danger).text_color(foreground))
                            .tooltip(move |window, cx| {
                                Tooltip::new(SharedString::from(format!(
                                    "Kill agent {id_for_tooltip}"
                                )))
                                .build(window, cx)
                            })
                            .child(SharedString::new_static("Kill"))
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                cx.stop_propagation();
                                state_for_kill.read(cx).submit_kill(id_for_kill.clone());
                            })
                            .into_any_element()
                    } else {
                        div().into_any_element()
                    };

                    // First row: status dot, id, runtime [· model],
                    // → Timeline, kill. `model` is omitted when None —
                    // rendering "· —" for an unknown model is just noise.
                    let head_meta = match &model {
                        Some(m) => format!("· {runtime} · {m}"),
                        None => format!("· {runtime}"),
                    };

                    // "→ Timeline" cross-link — navigates to Timeline
                    // pre-pinned to this agent. Same handoff slot the
                    // dashboard's agent-pin uses, so the user lands on
                    // Timeline already filtered to this agent. Renders
                    // for both running AND exited agents — the Timeline
                    // buffer covers historical events, so a recently-
                    // exited agent's calls are still useful to inspect.
                    let id_for_nav = a.id.clone();
                    let state_for_nav = agents_state.clone();
                    let timeline_link_id: gpui::ElementId =
                        a.derived.timeline_link_id.clone().into();
                    let timeline_btn = div()
                        .id(timeline_link_id)
                        .px_2()
                        .py_0p5()
                        .border_1()
                        .border_color(border)
                        .rounded_md()
                        .text_color(muted)
                        .cursor_pointer()
                        .hover(move |s| s.border_color(primary).text_color(primary))
                        .tooltip(|window, cx| {
                            Tooltip::new("Open Timeline filtered to this agent").build(window, cx)
                        })
                        .child(SharedString::new_static("\u{2192} Timeline"))
                        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                            cx.stop_propagation();
                            let id = id_for_nav.clone();
                            state_for_nav.update(cx, |s, cx| {
                                s.navigate_to_timeline_with_agent_pin(id);
                                cx.notify();
                            });
                        });

                    let head_row = h_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_color(dot_color)
                                .child(SharedString::from(dot.to_string())),
                        )
                        .child(div()
                                .text_color(foreground)
                                // a.id is SharedString — clone is a refcount
                                // bump, no alloc per render.
                                .child(a.id.clone()))
                        .child(div().text_color(muted).child(SharedString::from(head_meta)))
                        .child(timeline_btn)
                        .child(kill_btn);

                    // Second row: only chips that have real data.
                    // Previously this row rendered "pid — — 0.0 KiB log
                    // root: —" for any agent whose meta.json hadn't
                    // landed yet. Now each chip is conditional and we
                    // drop the row entirely if every chip is missing.
                    let mut meta_chips: Vec<SharedString> = Vec::with_capacity(4);
                    if let Some(p) = pid {
                        meta_chips.push(format!("pid {p}").into());
                    }
                    meta_chips.push(age.into());
                    meta_chips.push(log_label.into());
                    if let Some(r) = root_short {
                        meta_chips.push(SharedString::from(format!("root: {r}")));
                    }
                    let meta_row: gpui::AnyElement = if meta_chips.is_empty() {
                        div().into_any_element()
                    } else {
                        let mut row = h_flex().gap_3().text_color(muted);
                        for chip in meta_chips {
                            row = row.child(chip);
                        }
                        row.into_any_element()
                    };

                    // Prompt preview — always rendered. An empty preview
                    // shows "(no prompt recorded)" so the user knows the
                    // launch path was the dry-run / legacy one rather
                    // than the agent failing silently.
                    let prompt_text = if a.prompt_preview.trim().is_empty() {
                        SharedString::new_static("(no prompt recorded)")
                    } else {
                        SharedString::from(format!("\u{201C}{}\u{201D}", a.prompt_preview.clone()))
                    };
                    let prompt_color = if a.prompt_preview.trim().is_empty() {
                        muted
                    } else {
                        primary
                    };
                    let prompt_row: gpui::AnyElement = div()
                        .text_color(prompt_color)
                        .child(prompt_text)
                        .into_any_element();

                    // Expanded log-tail panel — only rendered when this
                    // card is the selection. Reads from `state.agent_log`
                    // and filters by id (defends against late results
                    // for a previous selection).
                    let log_panel: gpui::AnyElement = if is_selected {
                        let tail = agent_log.filter(|l| l.id == a.id).map(|l| match &l.result {
                            Ok(body) => log_tail(&body.body),
                            Err(e) => Err(format!("fetch failed: {e}")),
                        });
                        let entity_refresh = entity_for_refresh.clone();
                        let id_for_refresh = a.id.clone();
                        let refresh_btn = div()
                            .id(a.derived.log_refresh_id.clone())
                            .px_2()
                            .py_0p5()
                            .border_1()
                            .border_color(border)
                            .rounded_md()
                            .text_color(primary)
                            .cursor_pointer()
                            .hover(move |s| s.border_color(primary))
                            .tooltip(|window, cx| {
                                Tooltip::new("Re-fetch the log tail").build(window, cx)
                            })
                            .child(SharedString::new_static("Refresh"))
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                // Stop the click from bubbling to the
                                // outer card click handler (which would
                                // collapse the panel).
                                cx.stop_propagation();
                                let _ = id_for_refresh;
                                entity_refresh.update(cx, |this, cx| this.refresh_log(cx));
                            });
                        let header_row = h_flex()
                            .gap_3()
                            .child(
                                div()
                                    .text_color(muted)
                                    .child(SharedString::new_static("log tail")),
                            )
                            .child(refresh_btn);
                        let body_block: gpui::AnyElement = match tail {
                            None => div()
                                .text_color(muted)
                                .child(SharedString::new_static("fetching log…"))
                                .into_any_element(),
                            Some(Err(msg)) => div()
                                .text_color(cx.theme().danger)
                                .child(SharedString::from(msg))
                                .into_any_element(),
                            Some(Ok(body)) if body.is_empty() => div()
                                .text_color(muted)
                                .child(SharedString::new_static("(empty)"))
                                .into_any_element(),
                            Some(Ok(body)) => div()
                                .text_xs()
                                .text_color(foreground)
                                .child(SharedString::from(body))
                                .into_any_element(),
                        };
                        v_flex()
                            .mt_1()
                            .gap_1()
                            .px_2()
                            .py_2()
                            .border_1()
                            .border_color(border)
                            .rounded_md()
                            .bg(cx.theme().background)
                            .child(header_row)
                            .child(body_block)
                            .into_any_element()
                    } else {
                        div().into_any_element()
                    };

                    let card_border = if is_selected { primary } else { border };
                    let card_hover_bg = cx.theme().secondary;
                    let entity_click = entity_for_click.clone();
                    let id_for_click = a.id.clone();
                    v_flex()
                        .id(a.derived.card_id.clone())
                        .gap_1()
                        .px_3()
                        .py_2()
                        .border_1()
                        .border_color(card_border)
                        .rounded_md()
                        .cursor_pointer()
                        .hover(move |s| s.bg(card_hover_bg))
                        .child(head_row)
                        .child(meta_row)
                        .child(prompt_row)
                        .child(log_panel)
                        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                            let id = id_for_click.clone();
                            entity_click.update(cx, |this, cx| this.select_agent(id, cx));
                        })
                        .into_any_element()
                }))
                .into_any_element()
        };

        v_flex()
            .size_full()
            .child(header)
            .child(search_field)
            .child(pill_row)
            .child(div().flex_1().min_h(px(0.0)).child(body))
    }
}

/// Cheap "Xs ago" formatter — uses `last_event_ts` as the clock proxy
/// to avoid adding a real time crate just for one display string. The
/// drift vs wall-clock is at most one SSE poll interval, which is
/// invisible at this granularity.
///
/// Returns "just started" instead of "—" when started_ts is missing
/// (older runs without meta.json, or the brief race between RunDir
/// creation and write_meta) — the agent IS visible in the list, so
/// it just started; "—" suggests "data is broken" which it isn't.
fn relative_age(started_ts: i64, now_ts: Option<i64>) -> String {
    if started_ts == 0 || now_ts.is_none() {
        return "just started".into();
    }
    let now = now_ts.unwrap();
    let secs = (now - started_ts).max(0);
    if secs < 5 {
        "just started".into()
    } else if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

/// Human-friendly log-size chip. Empty logs get a plain "no output yet"
/// instead of "0.0 KiB log" — the size is what makes that string useful,
/// and zero is the absence of data, not a useful size.
fn format_log_size(bytes: u64) -> String {
    if bytes == 0 {
        return "no output yet".into();
    }
    if bytes < 1024 {
        return format!("{bytes} B log");
    }
    let kib = bytes as f64 / 1024.0;
    if kib < 1024.0 {
        return format!("{kib:.1} KiB log");
    }
    let mib = kib / 1024.0;
    format!("{mib:.1} MiB log")
}

/// Trim the log body to the last `LOG_TAIL_BYTES`, preserving UTF-8
/// boundaries by truncating to the next char boundary forward from the
/// raw cut point. Returns `Ok(String)` on the happy path; never fails
/// today but typed as Result to mirror the call site's error arm.
fn log_tail(body: &str) -> Result<String, String> {
    if body.len() <= LOG_TAIL_BYTES {
        return Ok(body.to_string());
    }
    let raw_cut = body.len() - LOG_TAIL_BYTES;
    // Walk forward from the raw cut to the next UTF-8 char boundary so
    // we never split a multibyte sequence.
    let mut cut = raw_cut;
    while !body.is_char_boundary(cut) {
        cut += 1;
    }
    Ok(format!("…{}", &body[cut..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── relative_age ─────────────────────────────────────────────────

    #[test]
    fn relative_age_unknown_started_returns_just_started() {
        // started_ts == 0 means meta.json hadn't landed; we still see
        // the run on disk, so "just started" is the truthful label.
        assert_eq!(relative_age(0, Some(1_700_000_000)), "just started");
    }

    #[test]
    fn relative_age_no_clock_proxy_returns_just_started() {
        assert_eq!(relative_age(1_700_000_000, None), "just started");
    }

    #[test]
    fn relative_age_under_5s_buckets_to_just_started() {
        assert_eq!(relative_age(1_000, Some(1_002)), "just started");
        assert_eq!(relative_age(1_000, Some(1_004)), "just started");
    }

    #[test]
    fn relative_age_seconds_minutes_hours_days() {
        assert_eq!(relative_age(1_000, Some(1_010)), "10s ago");
        assert_eq!(relative_age(1_000, Some(1_059)), "59s ago");
        assert_eq!(relative_age(1_000, Some(1_120)), "2m ago");
        assert_eq!(relative_age(1_000, Some(1_000 + 7_200)), "2h ago");
        assert_eq!(relative_age(1_000, Some(1_000 + 86_400 * 3)), "3d ago");
    }

    #[test]
    fn relative_age_negative_clock_skew_clamps_to_just_started() {
        // Clock skew between agent host and dashboard host can make
        // started_ts > now_ts; saturating to 0 prevents "-3s ago".
        assert_eq!(relative_age(2_000, Some(1_000)), "just started");
    }

    // ── format_log_size ─────────────────────────────────────────────

    #[test]
    fn format_log_size_zero_is_no_output_yet() {
        assert_eq!(format_log_size(0), "no output yet");
    }

    #[test]
    fn format_log_size_small_in_bytes() {
        assert_eq!(format_log_size(1), "1 B log");
        assert_eq!(format_log_size(512), "512 B log");
        assert_eq!(format_log_size(1023), "1023 B log");
    }

    #[test]
    fn format_log_size_kib() {
        assert_eq!(format_log_size(1024), "1.0 KiB log");
        assert_eq!(format_log_size(1024 * 12 + 256), "12.2 KiB log");
        assert_eq!(format_log_size(1024 * 1023), "1023.0 KiB log");
    }

    #[test]
    fn format_log_size_mib() {
        assert_eq!(format_log_size(1024 * 1024), "1.0 MiB log");
        assert_eq!(format_log_size(1024 * 1024 * 5 + 100 * 1024), "5.1 MiB log");
    }
}
