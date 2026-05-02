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

use gpui::{
    div, prelude::*, px, Context, Entity, Hsla, IntoElement, Render, SharedString, Window,
};
use gpui_component::{h_flex, v_flex, ActiveTheme};

use crate::state::AppState;

const KILLS_VISIBLE: usize = 8;

pub struct SystemRoute {
    state: Entity<AppState>,
}

impl SystemRoute {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self { state }
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
            .children(
                state
                    .last_error
                    .as_ref()
                    .map(|e| {
                        div()
                            .text_color(warning)
                            .child(SharedString::from(format!("• {e}")))
                    })
            );

        v_flex()
            .size_full()
            .px_5()
            .py_4()
            .gap_4()
            .child(header)
            .child(services_section(state, muted, border, success, danger))
            .child(otlp_section(state, muted, border, success, danger))
            .child(ollama_section(state, muted, border, success, danger))
            .child(profiles_section(state, muted, border, card))
            .child(models_section(state, muted, border, card))
            .child(kills_section(state, muted, border))
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

fn services_section(
    state: &AppState,
    muted: Hsla,
    border: Hsla,
    success: Hsla,
    danger: Hsla,
) -> gpui::Div {
    let body: gpui::AnyElement = match state.services.as_ref() {
        None => loading("loading services…", muted),
        Some(rep) => v_flex()
            .gap_1()
            .children(rep.services.iter().map(|s| {
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
                    .child(div().text_color(muted).child(SharedString::from(s.url.clone())))
                    .into_any_element()
            }))
            .into_any_element(),
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
                .child(div().text_color(muted).child(SharedString::from(h.endpoint.clone())))
                .children(
                    h.error
                        .as_ref()
                        .map(|e| {
                            div()
                                .text_color(danger)
                                .child(SharedString::from(format!("· {e}")))
                        })
                )
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
                .child(div().text_color(muted).child(SharedString::from(k.path.clone())))
                .children(
                    k.size_bytes
                        .map(|sz| {
                            div()
                                .text_color(muted)
                                .child(SharedString::from(format!("· {sz}B")))
                        })
                )
                .into_any_element()
        }
    };
    section("OLLAMA KEY", border, body)
}

fn profiles_section(state: &AppState, muted: Hsla, border: Hsla, card: Hsla) -> gpui::Div {
    let body: gpui::AnyElement = match state.agent_profiles.as_ref() {
        None => loading("loading profiles…", muted),
        Some(p) if p.profiles.is_empty() => loading("no profiles registered", muted),
        Some(p) => v_flex()
            .gap_1()
            .child(
                div()
                    .text_color(muted)
                    .child(SharedString::from(format!(
                        "{} profiles · {}",
                        p.profiles.len(),
                        p.dir
                    ))),
            )
            .child(
                v_flex().gap_1().children(p.profiles.iter().map(|prof| {
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
                        .children(
                            prof.crate_
                                .as_ref()
                                .map(|c| div().text_color(muted).child(SharedString::from(c.clone())))
                                )
                        .children(
                            prof.model
                                .as_ref()
                                .map(|m| {
                                    div().text_color(muted).child(SharedString::from(m.clone()))
                                })
                                )
                        .into_any_element()
                })),
            )
            .into_any_element(),
    };
    section("AGENT PROFILES", border, body)
}

fn models_section(state: &AppState, muted: Hsla, border: Hsla, card: Hsla) -> gpui::Div {
    let body: gpui::AnyElement = match state.agent_models.as_ref() {
        None => loading("loading models…", muted),
        Some(m) if m.models.is_empty() => loading("no models registered", muted),
        Some(m) => v_flex()
            .gap_1()
            .child(
                div()
                    .text_color(muted)
                    .child(SharedString::from(format!(
                        "{} models · {}",
                        m.models.len(),
                        m.dir
                    ))),
            )
            .child(
                v_flex().gap_1().children(m.models.iter().map(|model| {
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
                        .child(div().w(px(220.0)).child(SharedString::from(model.name.clone())))
                        .children(
                            model
                                .params
                                .as_ref()
                                .map(|p| {
                                    div()
                                        .text_color(muted)
                                        .child(SharedString::from(p.clone()))
                                })
                                )
                        .into_any_element()
                })),
            )
            .into_any_element(),
    };
    section("AGENT MODELS", border, body)
}

fn kills_section(state: &AppState, muted: Hsla, border: Hsla) -> gpui::Div {
    let body: gpui::AnyElement = match state.agent_kills.as_ref() {
        None => loading("loading kills…", muted),
        Some(k) if k.rows.is_empty() => loading("no recent kills — clean run", muted),
        Some(k) => v_flex()
            .gap_1()
            .children(k.rows.iter().take(KILLS_VISIBLE).map(|row| {
                h_flex()
                    .gap_3()
                    .child(div().w(px(80.0)).child(SharedString::from(row.run_id.clone())))
                    .child(
                        div()
                            .w(px(140.0))
                            .text_color(muted)
                            .child(SharedString::from(row.reason.clone())),
                    )
                    .children(
                        row.detail
                            .as_ref()
                            .map(|d| {
                                div().text_color(muted).child(SharedString::from(d.clone()))
                            })
                        )
                    .into_any_element()
            }))
            .into_any_element(),
    };
    section("RECENT KILLS", border, body)
}
