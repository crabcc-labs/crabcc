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

use std::collections::HashSet;

use crate::state::AppState;

const KILLS_VISIBLE: usize = 8;

/// Section keys for the per-section collapse state. Stable strings
/// so route re-renders see the same set across notify ticks.
mod section_keys {
    pub const SERVICES: &str = "services";
    pub const OTLP: &str = "otlp";
    pub const OLLAMA: &str = "ollama";
    pub const PROFILES: &str = "profiles";
    pub const MODELS: &str = "models";
    pub const KILLS: &str = "kills";
}

pub struct SystemRoute {
    state: Entity<AppState>,
    /// gpui-component InputState — owns text + focus for the filter.
    query_input: Entity<InputState>,
    /// Lower-cased mirror of the filter input value, kept in sync via
    /// `InputEvent::Change`. Avoids re-lowercasing on every render.
    query_lower: String,
    /// Per-section fold state. Default empty = every section
    /// expanded. Click a section header to toggle. Mirrors the
    /// timeline's `collapsed_agents` pattern: the route entity owns
    /// it (not AppState — UI affordance, not domain state).
    collapsed_sections: HashSet<&'static str>,
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
            collapsed_sections: HashSet::new(),
        }
    }

    fn toggle_section(&mut self, key: &'static str) {
        if !self.collapsed_sections.remove(key) {
            self.collapsed_sections.insert(key);
        }
    }

    fn is_section_collapsed(&self, key: &'static str) -> bool {
        self.collapsed_sections.contains(key)
    }
}

impl Render for SystemRoute {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let primary = cx.theme().primary;
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
        // Brighten the wrapper border to `primary` while focused —
        // gives the user a "you're typing here" cue without touching
        // gpui-component's own input chrome.
        let filter_focused = self
            .query_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        let filter_border = if filter_focused { primary } else { border };
        let filter_field = div()
            .border_1()
            .border_color(filter_border)
            .rounded_md()
            .px_2()
            .py_1()
            .child(Input::new(&self.query_input).appearance(false));

        let q = self.query_lower.as_str();
        let foreground = cx.theme().foreground;
        let entity = cx.entity();

        // Per-section collapse state — owned by the route entity.
        // Each section() call below picks up its own key's flag and
        // skips body rendering when collapsed.
        let services_collapsed = self.is_section_collapsed(section_keys::SERVICES);
        let otlp_collapsed = self.is_section_collapsed(section_keys::OTLP);
        let ollama_collapsed = self.is_section_collapsed(section_keys::OLLAMA);
        let profiles_collapsed = self.is_section_collapsed(section_keys::PROFILES);
        let models_collapsed = self.is_section_collapsed(section_keys::MODELS);
        let kills_collapsed = self.is_section_collapsed(section_keys::KILLS);

        // Per-section meta — small count summary visible in each
        // section header (whether expanded or collapsed). Health
        // signals (e.g. "1 down" for services) ride alongside so a
        // user can spot trouble without expanding every section.
        // None for sections where there's no useful count yet (OTLP /
        // Ollama are single-row state, captured in the body alone).
        let services_meta = state.services.as_ref().map(|r| {
            let total = r.services.len();
            let down = r.services.iter().filter(|s| !s.reachable).count();
            if down == 0 {
                format!("{total} services")
            } else {
                format!("{total} services · {down} down")
            }
        });
        let profiles_meta = state.agent_profiles.as_ref().map(|p| {
            let total = p.profiles.len();
            format!("{total} profile{}", if total == 1 { "" } else { "s" })
        });
        let models_meta = state.agent_models.as_ref().map(|m| {
            let total = m.models.len();
            format!("{total} model{}", if total == 1 { "" } else { "s" })
        });
        let kills_meta = state.agent_kills.as_ref().map(|k| {
            let total = k.rows.len();
            format!("{total} recent")
        });

        v_flex()
            .size_full()
            .px_5()
            .py_4()
            .gap_4()
            .child(header)
            .child(filter_field)
            .child(section(
                "SERVICE DISCOVERY",
                section_keys::SERVICES,
                services_collapsed,
                services_meta,
                border,
                muted,
                foreground,
                entity.clone(),
                services_body(state, muted, success, danger, q),
            ))
            .child(section(
                "OTLP HEALTH",
                section_keys::OTLP,
                otlp_collapsed,
                None,
                border,
                muted,
                foreground,
                entity.clone(),
                otlp_body(state, muted, success, danger),
            ))
            .child(section(
                "OLLAMA KEY",
                section_keys::OLLAMA,
                ollama_collapsed,
                None,
                border,
                muted,
                foreground,
                entity.clone(),
                ollama_body(state, muted, success, danger),
            ))
            .child(section(
                "AGENT PROFILES",
                section_keys::PROFILES,
                profiles_collapsed,
                profiles_meta,
                border,
                muted,
                foreground,
                entity.clone(),
                profiles_body(state, muted, card, q),
            ))
            .child(section(
                "AGENT MODELS",
                section_keys::MODELS,
                models_collapsed,
                models_meta,
                border,
                muted,
                foreground,
                entity.clone(),
                models_body(state, muted, card, q),
            ))
            .child(section(
                "RECENT KILLS",
                section_keys::KILLS,
                kills_collapsed,
                kills_meta,
                border,
                muted,
                foreground,
                entity,
                kills_body(state, self.state.clone(), muted, primary, card, q),
            ))
    }
}

/// Collapsible section header — click toggles via the route's
/// `toggle_section` method. Chevron + title; visible state of the
/// section body is controlled by the caller wrapping (or omitting)
/// the body child below.
#[allow(clippy::too_many_arguments)]
fn section_header(
    title: &'static str,
    key: &'static str,
    collapsed: bool,
    meta: Option<String>,
    muted: Hsla,
    foreground: Hsla,
    entity: Entity<SystemRoute>,
) -> gpui::Stateful<gpui::Div> {
    let chevron = if collapsed { "\u{25B8}" } else { "\u{25BE}" }; // ▸ / ▾
    let id = SharedString::from(format!("system-section-{key}"));
    let tooltip_text: SharedString = if collapsed {
        SharedString::from(format!("Expand {title}"))
    } else {
        SharedString::from(format!("Collapse {title}"))
    };
    let mut row = h_flex()
        .gap_2()
        .child(
            div()
                .text_color(muted)
                .child(SharedString::new_static(chevron)),
        )
        .child(
            div()
                .text_sm()
                .text_color(foreground)
                .child(SharedString::new_static(title)),
        );
    if let Some(m) = meta {
        // Count summary — visible whether expanded or collapsed so a
        // user scanning the route gets a glance "X services · Y down"
        // without expanding each section.
        row = row.child(
            div()
                .text_xs()
                .text_color(muted)
                .child(SharedString::from(format!("· {m}"))),
        );
    }
    div()
        .id(id)
        .px_1()
        .py_0p5()
        .rounded_md()
        .cursor_pointer()
        .hover(move |s| s.text_color(foreground))
        .tooltip(move |window, cx| Tooltip::new(tooltip_text.clone()).build(window, cx))
        .child(row)
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            entity.update(cx, |this, cx| {
                this.toggle_section(key);
                cx.notify();
            });
        })
}

#[allow(clippy::too_many_arguments)]
fn section(
    title: &'static str,
    key: &'static str,
    collapsed: bool,
    meta: Option<String>,
    border: Hsla,
    muted: Hsla,
    foreground: Hsla,
    entity: Entity<SystemRoute>,
    body: impl IntoElement,
) -> gpui::Div {
    let mut block = v_flex()
        .gap_2()
        .pb_3()
        .border_b_1()
        .border_color(border)
        .child(section_header(
            title, key, collapsed, meta, muted, foreground, entity,
        ));
    if !collapsed {
        block = block.child(body);
    }
    block
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

fn services_body(
    state: &AppState,
    muted: Hsla,
    success: Hsla,
    danger: Hsla,
    query_lower: &str,
) -> gpui::AnyElement {
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
                            .child(div().w(px(160.0)).child(s.name.clone()))
                            .child(
                                div()
                                    .w(px(80.0))
                                    .text_color(muted)
                                    .child(SharedString::from(format!("{}ms", s.latency_ms))),
                            )
                            .child(div().text_color(muted).child(s.url.clone()))
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
    body
}

fn otlp_body(state: &AppState, muted: Hsla, success: Hsla, danger: Hsla) -> gpui::AnyElement {
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
    body
}

fn ollama_body(state: &AppState, muted: Hsla, success: Hsla, danger: Hsla) -> gpui::AnyElement {
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
    body
}

fn profiles_body(state: &AppState, muted: Hsla, card: Hsla, query_lower: &str) -> gpui::AnyElement {
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
                                    .child(prof.id.clone()),
                            )
                            .children(
                                prof.crate_
                                    .as_ref()
                                    .map(|c| div().text_color(muted).child(c.clone())),
                            )
                            .children(
                                prof.model
                                    .as_ref()
                                    .map(|m| div().text_color(muted).child(m.clone())),
                            )
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
    body
}

fn models_body(state: &AppState, muted: Hsla, card: Hsla, query_lower: &str) -> gpui::AnyElement {
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
                                    .child(model.provider.clone()),
                            )
                            .child(div().w(px(220.0)).child(model.name.clone()))
                            .children(
                                model
                                    .params
                                    .as_ref()
                                    .map(|p| div().text_color(muted).child(p.clone())),
                            )
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
    body
}

fn kills_body(
    state: &AppState,
    state_entity: Entity<AppState>,
    muted: Hsla,
    primary: Hsla,
    secondary: Hsla,
    query_lower: &str,
) -> gpui::AnyElement {
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
                        // run_id is the agent run that was killed. Click
                        // navigates to Agents pre-selected so the user
                        // lands on the row whose log tail explains why.
                        // Same `navigate_to_agents_with_selection`
                        // handoff the Timeline's "→ Agents" pill uses.
                        let run_id_for_nav = row.run_id.clone();
                        let nav_state = state_entity.clone();
                        let id: gpui::ElementId =
                            SharedString::from(format!("kill-row-runid-{}", row.run_id)).into();
                        let run_id_cell = div()
                            .id(id)
                            .w(px(80.0))
                            .px_1()
                            .rounded_md()
                            .text_color(primary)
                            .cursor_pointer()
                            .hover(move |s| s.bg(secondary))
                            .child(row.run_id.clone())
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                let id = run_id_for_nav.clone();
                                nav_state.update(cx, |s, cx| {
                                    s.navigate_to_agents_with_selection(id);
                                    cx.notify();
                                });
                            });
                        h_flex()
                            .gap_3()
                            .child(run_id_cell)
                            .child(
                                div()
                                    .w(px(140.0))
                                    .text_color(muted)
                                    .child(row.reason.clone()),
                            )
                            .children(
                                row.detail
                                    .as_ref()
                                    .map(|d| div().text_color(muted).child(d.clone())),
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
    body
}
