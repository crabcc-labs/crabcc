//! System route — wires up the prefetched system-info surfaces in
//! one scrollable column:
//!
//!   * Service discovery (`/api/services`)
//!   * OTLP collector health (`/api/telemetry/otlp-health`)
//!   * Agent profiles (`/api/agent-profiles`)
//!   * Agent models (`/api/agent-models`)
//!   * Recent agent kills (`/api/agent-kills`)
//!   * Local Ollama API-key state (`/api/ollama-key`)
//!
//! Each section short-circuits with a "loading…" placeholder when the
//! underlying `AppState` slot is `None`. Errors land in
//! `AppState::last_error` (rendered next to the header) — they don't
//! prevent the other sections from drawing.
//!
//! A single top-of-route TextInput filters the long sections by
//! substring (case-insensitive). One input narrows everything; the
//! short single-row sections (OTLP, Ollama) aren't filtered. Per-
//! section match field lists:
//!
//! | Section          | Match against                                       |
//! |------------------|-----------------------------------------------------|
//! | services         | `name`, `url`                                       |
//! | agent_profiles   | `id`, `crate_?`, `description?`, `model?`           |
//! | agent_models     | `provider`, `name`, `params?`, `file`               |
//! | agent_kills      | `run_id`, `reason`, `detail?`                       |

use gpui::{div, prelude::*, px, Context, Entity, Hsla, IntoElement, Render, SharedString, Window};
use gpui_component::{
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex, ActiveTheme,
};

use crate::state::AppState;

const KILLS_VISIBLE: usize = 8;

pub struct SystemRoute {
    state: Entity<AppState>,
    /// gpui-component InputState — owns text + focus for the filter.
    query_input: Entity<InputState>,
    /// Lower-cased mirror of the filter input value, kept in sync via
    /// `InputEvent::Change`. Avoids re-lowercasing on every render.
    query_lower: String,
}

impl SystemRoute {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        let query_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter visible rows…"));
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
}

impl Render for SystemRoute {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let success = cx.theme().success;
        let danger = cx.theme().danger;
        let warning = cx.theme().warning;
        let card = cx.theme().secondary;

        let header = h_flex()
            .gap_3()
            .child(div().text_lg().child(SharedString::new_static("System")))
            .children(state.last_error.as_ref().map(|e| {
                div()
                    .text_color(warning)
                    .child(SharedString::from(format!("• {e}")))
            }));

        // Single filter input above all sections — narrows services
        // / profiles / models / kills uniformly. Sits below the
        // header so it's visible without scrolling, no matter which
        // section the user's eye is on.
        let filter_field = div()
            .border_1()
            .border_color(border)
            .rounded_md()
            .px_2()
            .py_1()
            .child(Input::new(&self.query_input).appearance(false));

        let q = self.query_lower.as_str();

        v_flex()
            .size_full()
            .px_5()
            .py_4()
            .gap_4()
            .child(header)
            .child(filter_field)
            .child(services_section(state, muted, border, success, danger, q))
            .child(otlp_section(state, muted, border, success, danger))
            .child(ollama_section(state, muted, border, success, danger))
            .child(profiles_section(state, muted, border, card, q))
            .child(models_section(state, muted, border, card, q))
            .child(kills_section(state, muted, border, q))
    }
}

fn section(title: &'static str, border: Hsla, body: impl IntoElement) -> gpui::Div {
    v_flex()
        .gap_2()
        .pb_3()
        .border_b_1()
        .border_color(border)
        .child(div().text_sm().child(SharedString::new_static(title)))
        .child(body)
}

fn loading(text: &'static str, muted: Hsla) -> gpui::AnyElement {
    div()
        .text_color(muted)
        .child(SharedString::new_static(text))
        .into_any_element()
}

/// Render the count-line that sits above each filterable section's
/// list — only when the filter is active. Keeps the no-filter happy
/// path visually compact.
fn count_line(visible: usize, total: usize, query_lower: &str, muted: Hsla) -> gpui::AnyElement {
    if query_lower.is_empty() {
        div().into_any_element()
    } else {
        div()
            .text_color(muted)
            .child(SharedString::from(format!("{visible} of {total} match")))
            .into_any_element()
    }
}

/// Distinct empty-state for "filter mismatched everything" — keeps a
/// typo from looking like the data slot is empty.
fn no_match(noun: &str, query_lower: &str, muted: Hsla) -> gpui::AnyElement {
    div()
        .text_color(muted)
        .child(SharedString::from(format!(
            "no {noun} match \u{201C}{query_lower}\u{201D}"
        )))
        .into_any_element()
}

fn services_section(
    state: &AppState,
    muted: Hsla,
    border: Hsla,
    success: Hsla,
    danger: Hsla,
    query_lower: &str,
) -> gpui::Div {
    let body: gpui::AnyElement = match state.services.as_ref() {
        None => loading("loading services…", muted),
        Some(rep) => {
            let total = rep.services.len();
            let visible: Vec<_> = rep
                .services
                .iter()
                .filter(|s| {
                    if query_lower.is_empty() {
                        return true;
                    }
                    s.name.to_lowercase().contains(query_lower)
                        || s.url.to_lowercase().contains(query_lower)
                })
                .collect();
            let visible_count = visible.len();
            let list: gpui::AnyElement = if visible.is_empty() && !query_lower.is_empty() {
                no_match("services", query_lower, muted)
            } else {
                v_flex()
                    .gap_1()
                    .children(visible.into_iter().map(|s| {
                        let (mark, color) = if s.reachable {
                            ("✓", success)
                        } else {
                            ("✗", danger)
                        };
                        h_flex()
                            .gap_3()
                            .child(
                                div()
                                    .w(px(20.0))
                                    .text_color(color)
                                    .child(SharedString::from(mark.to_string())),
                            )
                            .child(div().w(px(160.0)).child(SharedString::from(s.name.clone())))
                            .child(
                                div()
                                    .w(px(80.0))
                                    .text_color(muted)
                                    .child(SharedString::from(format!("{}ms", s.latency_ms))),
                            )
                            .child(
                                div()
                                    .text_color(muted)
                                    .child(SharedString::from(s.url.clone())),
                            )
                            .into_any_element()
                    }))
                    .into_any_element()
            };
            v_flex()
                .gap_1()
                .child(count_line(visible_count, total, query_lower, muted))
                .child(list)
                .into_any_element()
        }
    };
    section("SERVICE DISCOVERY", border, body)
}

fn otlp_section(
    state: &AppState,
    muted: Hsla,
    border: Hsla,
    success: Hsla,
    danger: Hsla,
) -> gpui::Div {
    let body: gpui::AnyElement = match state.otlp_health.as_ref() {
        None => loading("loading OTLP health…", muted),
        Some(h) => {
            let (mark, color) = if h.reachable {
                ("✓ reachable", success)
            } else {
                ("✗ unreachable", danger)
            };
            h_flex()
                .gap_3()
                .child(
                    div()
                        .text_color(color)
                        .w(px(140.0))
                        .child(SharedString::from(mark.to_string())),
                )
                .child(
                    div()
                        .text_color(muted)
                        .child(SharedString::from(h.endpoint.clone())),
                )
                .children(h.error.as_ref().map(|e| {
                    div()
                        .text_color(danger)
                        .child(SharedString::from(format!("· {e}")))
                }))
                .into_any_element()
        }
    };
    section("OTLP HEALTH", border, body)
}

fn ollama_section(
    state: &AppState,
    muted: Hsla,
    border: Hsla,
    success: Hsla,
    danger: Hsla,
) -> gpui::Div {
    let body: gpui::AnyElement = match state.ollama_key.as_ref() {
        None => loading("loading ollama key…", muted),
        Some(k) => {
            let (mark, color) = if k.present {
                ("✓ present", success)
            } else {
                ("✗ missing", danger)
            };
            h_flex()
                .gap_3()
                .child(
                    div()
                        .text_color(color)
                        .w(px(140.0))
                        .child(SharedString::from(mark.to_string())),
                )
                .child(
                    div()
                        .text_color(muted)
                        .child(SharedString::from(k.path.clone())),
                )
                .children(k.size_bytes.map(|sz| {
                    div()
                        .text_color(muted)
                        .child(SharedString::from(format!("· {sz}B")))
                }))
                .into_any_element()
        }
    };
    section("OLLAMA KEY", border, body)
}

fn profiles_section(
    state: &AppState,
    muted: Hsla,
    border: Hsla,
    card: Hsla,
    query_lower: &str,
) -> gpui::Div {
    let body: gpui::AnyElement = match state.agent_profiles.as_ref() {
        None => loading("loading profiles…", muted),
        Some(p) if p.profiles.is_empty() => loading("no profiles registered", muted),
        Some(p) => {
            let total = p.profiles.len();
            let visible: Vec<_> = p
                .profiles
                .iter()
                .filter(|prof| {
                    if query_lower.is_empty() {
                        return true;
                    }
                    if prof.id.to_lowercase().contains(query_lower) {
                        return true;
                    }
                    if let Some(c) = prof.crate_.as_ref() {
                        if c.to_lowercase().contains(query_lower) {
                            return true;
                        }
                    }
                    if let Some(d) = prof.description.as_ref() {
                        if d.to_lowercase().contains(query_lower) {
                            return true;
                        }
                    }
                    prof.model
                        .as_ref()
                        .is_some_and(|m| m.to_lowercase().contains(query_lower))
                })
                .collect();
            let visible_count = visible.len();
            let list: gpui::AnyElement = if visible.is_empty() && !query_lower.is_empty() {
                no_match("profiles", query_lower, muted)
            } else {
                v_flex()
                    .gap_1()
                    .children(visible.into_iter().map(|prof| {
                        h_flex()
                            .gap_3()
                            .child(
                                div()
                                    .px_2()
                                    .py_0p5()
                                    .bg(card)
                                    .rounded_md()
                                    .child(SharedString::from(prof.id.clone())),
                            )
                            .children(prof.crate_.as_ref().map(|c| {
                                div().text_color(muted).child(SharedString::from(c.clone()))
                            }))
                            .children(prof.model.as_ref().map(|m| {
                                div().text_color(muted).child(SharedString::from(m.clone()))
                            }))
                            .into_any_element()
                    }))
                    .into_any_element()
            };
            v_flex()
                .gap_1()
                .child(if query_lower.is_empty() {
                    div().text_color(muted).child(SharedString::from(format!(
                        "{} profiles · {}",
                        total, p.dir
                    )))
                } else {
                    div().text_color(muted).child(SharedString::from(format!(
                        "{visible_count} of {total} match · {}",
                        p.dir
                    )))
                })
                .child(list)
                .into_any_element()
        }
    };
    section("AGENT PROFILES", border, body)
}

fn models_section(
    state: &AppState,
    muted: Hsla,
    border: Hsla,
    card: Hsla,
    query_lower: &str,
) -> gpui::Div {
    let body: gpui::AnyElement = match state.agent_models.as_ref() {
        None => loading("loading models…", muted),
        Some(m) if m.models.is_empty() => loading("no models registered", muted),
        Some(m) => {
            let total = m.models.len();
            let visible: Vec<_> = m
                .models
                .iter()
                .filter(|model| {
                    if query_lower.is_empty() {
                        return true;
                    }
                    if model.provider.to_lowercase().contains(query_lower) {
                        return true;
                    }
                    if model.name.to_lowercase().contains(query_lower) {
                        return true;
                    }
                    if model.file.to_lowercase().contains(query_lower) {
                        return true;
                    }
                    model
                        .params
                        .as_ref()
                        .is_some_and(|p| p.to_lowercase().contains(query_lower))
                })
                .collect();
            let visible_count = visible.len();
            let list: gpui::AnyElement = if visible.is_empty() && !query_lower.is_empty() {
                no_match("models", query_lower, muted)
            } else {
                v_flex()
                    .gap_1()
                    .children(visible.into_iter().map(|model| {
                        h_flex()
                            .gap_3()
                            .child(
                                div()
                                    .w(px(80.0))
                                    .px_2()
                                    .py_0p5()
                                    .bg(card)
                                    .rounded_md()
                                    .child(SharedString::from(model.provider.clone())),
                            )
                            .child(
                                div()
                                    .w(px(220.0))
                                    .child(SharedString::from(model.name.clone())),
                            )
                            .children(model.params.as_ref().map(|p| {
                                div().text_color(muted).child(SharedString::from(p.clone()))
                            }))
                            .into_any_element()
                    }))
                    .into_any_element()
            };
            v_flex()
                .gap_1()
                .child(if query_lower.is_empty() {
                    div()
                        .text_color(muted)
                        .child(SharedString::from(format!("{} models · {}", total, m.dir)))
                } else {
                    div().text_color(muted).child(SharedString::from(format!(
                        "{visible_count} of {total} match · {}",
                        m.dir
                    )))
                })
                .child(list)
                .into_any_element()
        }
    };
    section("AGENT MODELS", border, body)
}

fn kills_section(state: &AppState, muted: Hsla, border: Hsla, query_lower: &str) -> gpui::Div {
    let body: gpui::AnyElement = match state.agent_kills.as_ref() {
        None => loading("loading kills…", muted),
        Some(k) if k.rows.is_empty() => loading("no recent kills — clean run", muted),
        Some(k) => {
            // Filter first, then cap. A targeted query can surface
            // older kills that would otherwise be off the bottom of
            // the 8-row visible window.
            let total = k.rows.len();
            let visible: Vec<_> = k
                .rows
                .iter()
                .filter(|row| {
                    if query_lower.is_empty() {
                        return true;
                    }
                    if row.run_id.to_lowercase().contains(query_lower) {
                        return true;
                    }
                    if row.reason.to_lowercase().contains(query_lower) {
                        return true;
                    }
                    row.detail
                        .as_ref()
                        .is_some_and(|d| d.to_lowercase().contains(query_lower))
                })
                .take(KILLS_VISIBLE)
                .collect();
            let visible_count = visible.len();
            let list: gpui::AnyElement = if visible.is_empty() && !query_lower.is_empty() {
                no_match("kills", query_lower, muted)
            } else {
                v_flex()
                    .gap_1()
                    .children(visible.into_iter().map(|row| {
                        h_flex()
                            .gap_3()
                            .child(
                                div()
                                    .w(px(80.0))
                                    .child(SharedString::from(row.run_id.clone())),
                            )
                            .child(
                                div()
                                    .w(px(140.0))
                                    .text_color(muted)
                                    .child(SharedString::from(row.reason.clone())),
                            )
                            .children(row.detail.as_ref().map(|d| {
                                div().text_color(muted).child(SharedString::from(d.clone()))
                            }))
                            .into_any_element()
                    }))
                    .into_any_element()
            };
            v_flex()
                .gap_1()
                .child(count_line(visible_count, total, query_lower, muted))
                .child(list)
                .into_any_element()
        }
    };
    section("RECENT KILLS", border, body)
}
