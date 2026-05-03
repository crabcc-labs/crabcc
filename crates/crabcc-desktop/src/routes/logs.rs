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

use gpui::{div, prelude::*, px, Context, Entity, Hsla, IntoElement, Render, SharedString, Window};
use gpui_component::{
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex, ActiveTheme,
};
use serde_json::Value;

use crate::api::types::{LogLevel, TelemetryEvent};
use crate::state::AppState;

const VISIBLE_ROWS: usize = 80;

pub struct LogsRoute {
    state: Entity<AppState>,
    /// gpui-component InputState — owns text + focus for the filter.
    query_input: Entity<InputState>,
    /// Lower-cased mirror of the input value, kept in sync via
    /// `InputEvent::Change`. Avoids re-lowercasing the query for
    /// every match check on every render.
    query_lower: String,
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
        }
    }

    fn event_matches(&self, evt: &TelemetryEvent) -> bool {
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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

        let search_field = div()
            .border_1()
            .border_color(border)
            .rounded_md()
            .px_2()
            .py_1()
            .child(Input::new(&self.query_input).appearance(false));

        let body: gpui::AnyElement = if state.telemetry.is_empty() {
            div()
                .text_color(muted)
                .min_h(px(60.0))
                .child(SharedString::new_static(
                    "no events yet — telemetry poller fires every 3s",
                ))
                .into_any_element()
        } else if visible.is_empty() {
            // Telemetry has rows but none match the filter. Distinct
            // copy from the empty-tail state so a typo doesn't read
            // as "the poller is dead".
            div()
                .text_color(muted)
                .min_h(px(60.0))
                .child(SharedString::from(format!(
                    "no events match \u{201C}{}\u{201D}",
                    self.query_lower
                )))
                .into_any_element()
        } else {
            v_flex()
                .gap_1()
                .children(
                    visible
                        .into_iter()
                        .map(|evt| {
                            row(evt, muted, border, info, warning, danger).into_any_element()
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
            .child(body)
    }
}

fn row(
    evt: &TelemetryEvent,
    muted: Hsla,
    border: Hsla,
    info: Hsla,
    warning: Hsla,
    danger: Hsla,
) -> gpui::Div {
    let level_color = match evt.level {
        LogLevel::Trace | LogLevel::Debug => muted,
        LogLevel::Info => info,
        LogLevel::Warn => warning,
        LogLevel::Error => danger,
    };

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
        .child(
            div()
                .w(px(60.0))
                .text_color(level_color)
                .child(SharedString::from(level_label(evt.level))),
        )
        // Target (e.g. `crabcc::core::store`). Truncated to keep the
        // body column wide.
        .child(
            div()
                .w(px(220.0))
                .text_color(muted)
                .child(SharedString::from(truncate(&evt.target, 32))),
        )
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

/// Pretty time-of-day from a unix-seconds timestamp. UTC; formatting in
/// the user's local zone needs a date crate (`chrono` / `time`) and
/// isn't worth the dep weight for a developer-facing log tail.
fn format_time(unix_seconds: i64) -> String {
    let day = unix_seconds.rem_euclid(86_400);
    let h = day / 3600;
    let m = (day / 60) % 60;
    let s = day % 60;
    format!("{h:02}:{m:02}:{s:02}")
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
