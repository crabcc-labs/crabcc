//! Logs route — scrolling tail of `/api/telemetry`.
//!
//! Driven by the telemetry polling worker in `state::spawn_workers`
//! (3-second tick). The view observes `AppState`, renders the latest
//! ~256 events newest-first, and colours level badges from the gpui
//! theme (`info` / `warning` / `danger`, with TRACE/DEBUG falling
//! back to `muted_foreground`).
//!
//! Top-of-route TextInput filters events as the user types — matches
//! against target (e.g. `crabcc::core::store`) and the rendered
//! message preview (case-insensitive). Mirrors the Agents / Knowledge
//! filter pattern.

use gpui::{
    div, prelude::*, px, Context, Entity, Focusable, Hsla, IntoElement, MouseButton, Render,
    SharedString, Window,
};
use gpui_component::{
    h_flex,
    input::{Input, InputEvent, InputState},
    tooltip::Tooltip,
    v_flex, ActiveTheme,
};
use serde_json::Value;

use crate::api::types::{LogLevel, TelemetryEvent};
use crate::routes::empty::empty_state;
use crate::routes::time::format_time;
use crate::state::AppState;

const VISIBLE_ROWS: usize = 80;

/// Level filter pill — `All` matches every level, the rest narrow to
/// one. ANDed with the substring filter at match time so a user can
/// drill in: "ERROR rows whose target contains store" works in two
/// interactions. Kept on the route entity (not `AppState`) — same
/// call as the substring filter; UI affordance, not domain state.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LevelFilter {
    All,
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LevelFilter {
    const ALL: [LevelFilter; 6] = [
        LevelFilter::All,
        LevelFilter::Trace,
        LevelFilter::Debug,
        LevelFilter::Info,
        LevelFilter::Warn,
        LevelFilter::Error,
    ];

    fn label(self) -> &'static str {
        match self {
            LevelFilter::All => "All",
            LevelFilter::Trace => "TRACE",
            LevelFilter::Debug => "DEBUG",
            LevelFilter::Info => "INFO",
            LevelFilter::Warn => "WARN",
            LevelFilter::Error => "ERROR",
        }
    }

    fn id(self) -> &'static str {
        match self {
            LevelFilter::All => "logs-pill-all",
            LevelFilter::Trace => "logs-pill-trace",
            LevelFilter::Debug => "logs-pill-debug",
            LevelFilter::Info => "logs-pill-info",
            LevelFilter::Warn => "logs-pill-warn",
            LevelFilter::Error => "logs-pill-error",
        }
    }

    fn matches(self, evt: &TelemetryEvent) -> bool {
        match self {
            LevelFilter::All => true,
            LevelFilter::Trace => matches!(evt.level, LogLevel::Trace),
            LevelFilter::Debug => matches!(evt.level, LogLevel::Debug),
            LevelFilter::Info => matches!(evt.level, LogLevel::Info),
            LevelFilter::Warn => matches!(evt.level, LogLevel::Warn),
            LevelFilter::Error => matches!(evt.level, LogLevel::Error),
        }
    }

    fn from_log_level(level: LogLevel) -> Self {
        match level {
            LogLevel::Trace => LevelFilter::Trace,
            LogLevel::Debug => LevelFilter::Debug,
            LogLevel::Info => LevelFilter::Info,
            LogLevel::Warn => LevelFilter::Warn,
            LogLevel::Error => LevelFilter::Error,
        }
    }
}

pub struct LogsRoute {
    state: Entity<AppState>,
    /// gpui-component InputState — owns text + focus for the filter.
    query_input: Entity<InputState>,
    /// Lower-cased mirror of the input value, kept in sync via
    /// `InputEvent::Change`. Avoids re-lowercasing the query for
    /// every match check on every render.
    query_lower: String,
    /// Level pill state, ANDed with `query_lower` for visibility.
    level_filter: LevelFilter,
    /// Active target-pin (None = all targets). Set by clicking a
    /// row's `target` cell, cleared by clicking the same target or
    /// the header pill. Exact-match on the full module path so the
    /// user doesn't need to type the namespace into the substring
    /// filter to drill into one module's logs.
    target_pin: Option<SharedString>,
}

impl LogsRoute {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        let query_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter by target / message…"));
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
            level_filter: LevelFilter::All,
            target_pin: None,
        }
    }

    fn toggle_target_pin(&mut self, target: SharedString) {
        if self.target_pin.as_deref() == Some(target.as_ref()) {
            self.target_pin = None;
        } else {
            self.target_pin = Some(target);
        }
    }

    fn event_matches(&self, evt: &TelemetryEvent) -> bool {
        if !self.level_filter.matches(evt) {
            return false;
        }
        if let Some(pin) = self.target_pin.as_deref() {
            if evt.target.as_ref() != pin {
                return false;
            }
        }
        if self.query_lower.is_empty() {
            return true;
        }
        let q = self.query_lower.as_str();
        if evt.target.to_lowercase().contains(q) {
            return true;
        }
        preview(&evt.fields).to_lowercase().contains(q)
    }
}

impl Render for LogsRoute {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let info = cx.theme().info;
        let warning = cx.theme().warning;
        let danger = cx.theme().danger;

        // Filter applies to the live tail; collect the visible slice
        // once so the header count and the body iterator agree.
        let total = state.telemetry.len();
        let visible: Vec<&TelemetryEvent> = state
            .telemetry
            .iter()
            .rev()
            .filter(|e| self.event_matches(e))
            .take(VISIBLE_ROWS)
            .collect();
        let visible_count = visible.len();

        let count_label = if self.query_lower.is_empty() {
            format!(
                "{total} events · cursor {} · poll 3s",
                state.telemetry_cursor
            )
        } else {
            format!(
                "{visible_count} of {total} match · cursor {} · poll 3s",
                state.telemetry_cursor
            )
        };
        let header = h_flex()
            .gap_3()
            .child(div().text_lg().child(SharedString::new_static("Logs")))
            .child(
                div()
                    .text_color(muted)
                    .child(SharedString::from(count_label)),
            );

        // Brighten the wrapper border to `primary` while focused —
        // gives the user a visible "you're typing here" cue without
        // touching gpui-component's own input chrome.
        let primary = cx.theme().primary;
        let filter_focused = self
            .query_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        let filter_border = if filter_focused { primary } else { border };
        let search_field = div()
            .border_1()
            .border_color(filter_border)
            .rounded_md()
            .px_2()
            .py_1()
            .child(Input::new(&self.query_input).appearance(false));

        // ── Level pills ────────────────────────────────────────────
        let foreground = cx.theme().foreground;
        let active_filter = self.level_filter;
        let entity_for_pill = cx.entity();
        let pill_hover_bg = cx.theme().secondary;
        let level_pills = LevelFilter::ALL.into_iter().map(|f| {
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
                .hover(move |s| s.bg(pill_hover_bg))
                .child(SharedString::new_static(f.label()))
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    entity.update(cx, |this, cx| {
                        this.level_filter = f;
                        cx.notify();
                    });
                })
                .into_any_element()
        });
        // Target-pin clear pill — only renders when an active target
        // is pinned, mirroring the dashboard's pin-pill pattern. Click
        // clears the pin without having to scroll to find a row carrying
        // the pinned target (which would scroll off as new logs arrive).
        let target_clear_pill: Option<gpui::AnyElement> = self.target_pin.clone().map(|t| {
            let entity_for_clear = cx.entity();
            let label = SharedString::from(format!("{} \u{00D7}", truncate(&t, 32)));
            div()
                .id("logs-target-pin-clear")
                .px_2()
                .py_0p5()
                .border_1()
                .border_color(primary)
                .rounded_md()
                .text_color(primary)
                .cursor_pointer()
                .hover(move |s| s.bg(pill_hover_bg))
                .child(label)
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    entity_for_clear.update(cx, |this, cx| {
                        this.target_pin = None;
                        cx.notify();
                    });
                })
                .into_any_element()
        });
        let pill_row = h_flex()
            .gap_2()
            .children(level_pills)
            .children(target_clear_pill);

        let body: gpui::AnyElement = if state.telemetry.is_empty() {
            empty_state(
                "\u{25CC}",
                "No events yet",
                "Telemetry poller fires every 3s — events will appear here.",
                muted,
                cx.theme().foreground,
            )
            .into_any_element()
        } else if visible.is_empty() {
            // Telemetry has rows but none match the filter(s). Distinct
            // copy from the empty-tail state so the user doesn't read
            // it as "the poller is dead". Description mentions
            // whichever filters are currently narrowing the view.
            let mut bits: Vec<String> = Vec::new();
            if self.level_filter != LevelFilter::All {
                bits.push(format!("level {}", self.level_filter.label()));
            }
            if let Some(t) = self.target_pin.as_deref() {
                bits.push(format!("target {t}"));
            }
            if !self.query_lower.is_empty() {
                bits.push(format!("\u{201C}{}\u{201D}", self.query_lower));
            }
            let what = if bits.is_empty() {
                // Defensive — shouldn't fire since the no-filter
                // visible-is-empty case is already covered above.
                "current filters".to_string()
            } else {
                bits.join(" + ")
            };
            empty_state(
                "\u{1F50D}",
                "No events match the filter",
                &format!("Nothing matches {what} — try widening the level or query."),
                muted,
                cx.theme().foreground,
            )
            .into_any_element()
        } else {
            // Capture the entity once outside the per-row map so each
            // row's level-badge click handler can update the route's
            // `level_filter` without recomputing on every iteration.
            let entity = cx.entity();
            let active_level = self.level_filter;
            let target_pin = self.target_pin.clone();
            v_flex()
                .gap_1()
                .children(
                    visible
                        .into_iter()
                        .enumerate()
                        .map(|(idx, evt)| {
                            row(
                                idx,
                                evt,
                                muted,
                                border,
                                info,
                                warning,
                                danger,
                                primary,
                                foreground,
                                active_level,
                                target_pin.as_ref(),
                                entity.clone(),
                            )
                            .into_any_element()
                        })
                        .collect::<Vec<_>>(),
                )
                .into_any_element()
        };

        v_flex()
            .size_full()
            .px_5()
            .py_4()
            .gap_3()
            .child(header)
            .child(search_field)
            .child(pill_row)
            .child(body)
    }
}

#[allow(clippy::too_many_arguments)]
fn row(
    idx: usize,
    evt: &TelemetryEvent,
    muted: Hsla,
    border: Hsla,
    info: Hsla,
    warning: Hsla,
    danger: Hsla,
    primary: Hsla,
    foreground: Hsla,
    active_level: LevelFilter,
    target_pin: Option<&SharedString>,
    entity: Entity<LogsRoute>,
) -> gpui::Div {
    let level_color = match evt.level {
        LogLevel::Trace | LogLevel::Debug => muted,
        LogLevel::Info => info,
        LogLevel::Warn => warning,
        LogLevel::Error => danger,
    };

    // Click target — clicking a level badge sets that level as the
    // active filter (toggles back to `All` if the same level is
    // already pinned). gpui requires stateful elements (those with
    // an `id`) to declare it; the rendered slice index is unique
    // within a single render pass, so suffixing with `idx` is
    // sufficient and avoids a String allocation per event.
    let target_filter = LevelFilter::from_log_level(evt.level);
    let level_pinned = active_level == target_filter;
    let badge_id: gpui::ElementId = SharedString::from(format!("logs-row-level-{idx}")).into();

    h_flex()
        .gap_3()
        .py_1()
        .border_b_1()
        .border_color(border)
        // Time-of-day, UTC. Local-zone formatting needs `chrono`; deferred.
        .child(
            div()
                .w(px(72.0))
                .text_color(muted)
                .child(SharedString::from(format_time(evt.ts))),
        )
        // Level badge — fixed-width so columns align across rows.
        // Becomes a click target when the route is mounted; pinned
        // levels render with the primary border + foreground colour
        // so the user can spot the active filter even after pills
        // scrolled off-screen.
        .child(
            div()
                .id(badge_id)
                .w(px(60.0))
                .px_1()
                .border_1()
                .border_color(if level_pinned {
                    primary
                } else {
                    gpui::transparent_black()
                })
                .rounded_md()
                .text_color(level_color)
                .cursor_pointer()
                .hover(move |s| s.border_color(primary))
                .tooltip(|window, cx| {
                    Tooltip::new("Pin / unpin level filter").build(window, cx)
                })
                .child(SharedString::from(level_label(evt.level)))
                .on_mouse_down(MouseButton::Left, {
                    let entity = entity.clone();
                    move |_, _, cx| {
                        entity.update(cx, |this, cx| {
                            // Toggle: same level → All; otherwise pin.
                            this.level_filter = if this.level_filter == target_filter {
                                LevelFilter::All
                            } else {
                                target_filter
                            };
                            cx.notify();
                        });
                    }
                }),
        )
        // Target (e.g. `crabcc::core::store`). Truncated to keep the
        // body column wide. Click toggles a target-pin: a stronger
        // narrow than the substring filter (exact match on the full
        // path) which is what you usually want when drilling into one
        // module's logs. Pinned target renders with `foreground` for
        // affordance, mirroring the level-badge pattern above.
        .child({
            let target_id: gpui::ElementId =
                SharedString::from(format!("logs-row-target-{idx}")).into();
            let target_clicked = evt.target.clone();
            let entity_for_target = entity.clone();
            let target_active =
                target_pin.is_some_and(|p| p.as_ref() == evt.target.as_ref());
            div()
                .id(target_id)
                .w(px(220.0))
                .text_color(if target_active { foreground } else { muted })
                .child(SharedString::from(truncate(&evt.target, 32)))
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    let t = target_clicked.clone();
                    entity_for_target.update(cx, |this, cx| {
                        this.toggle_target_pin(t);
                        cx.notify();
                    });
                })
        })
        // Message preview from `fields.message` if present, else a
        // compact JSON dump.
        .child(SharedString::from(truncate(&preview(&evt.fields), 240)))
}

fn level_label(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Trace => "TRACE",
        LogLevel::Debug => "DEBUG",
        LogLevel::Info => "INFO",
        LogLevel::Warn => "WARN",
        LogLevel::Error => "ERROR",
    }
}

fn preview(fields: &Value) -> String {
    // Tracing's structured JSON usually carries the human message under
    // `message`. Fall back to the whole compact JSON if not.
    if let Some(msg) = fields.get("message").and_then(|m| m.as_str()) {
        return msg.to_string();
    }
    serde_json::to_string(fields).unwrap_or_default()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_formatter_pads_components() {
        assert_eq!(format_time(0), "00:00:00");
        assert_eq!(format_time(3661), "01:01:01");
        assert_eq!(format_time(86_399), "23:59:59");
    }

    #[test]
    fn preview_prefers_message_field() {
        let v: Value = serde_json::from_str(r#"{"message":"hello","extra":1}"#).unwrap();
        assert_eq!(preview(&v), "hello");
    }

    #[test]
    fn preview_falls_back_to_json() {
        let v: Value = serde_json::from_str(r#"{"foo":1}"#).unwrap();
        assert_eq!(preview(&v), r#"{"foo":1}"#);
    }

    #[test]
    fn truncate_appends_ellipsis() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 6), "hello…");
    }
}
