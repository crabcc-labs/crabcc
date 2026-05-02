//! Knowledge route — memory drawer browser + ingest form.
//!
//! Reads from `AppState::memory_recent` (refreshed every 10s by the
//! memory poller). The form at the top POSTs to `/api/memory/ingest`
//! and pushes a follow-up `MemoryRefresh` so the new drawer appears
//! immediately rather than waiting up to 10s for the next poll.

use gpui::{
    div, prelude::*, px, Context, Entity, Hsla, IntoElement, MouseButton, Render, SharedString,
    Window,
};
use gpui_component::{
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex, ActiveTheme,
};

use crate::api::types::{MemoryDrawer, MemoryIngestRequest};
use crate::state::AppState;

const VISIBLE_DRAWERS: usize = 50;
const INGEST_SOURCE: &str = "desktop:ingest";

pub struct KnowledgeRoute {
    state: Entity<AppState>,
    ingest_input: Entity<InputState>,
    /// Live mirror of the input's text — read on submit so the click
    /// handler doesn't need to crack open the entity again.
    pending_text: String,
}

impl KnowledgeRoute {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        let ingest_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Ingest a note (text)…"));
        cx.subscribe_in(&ingest_input, window, |this, state, event, _, cx| {
            match event {
                InputEvent::Change => {
                    this.pending_text = state.read(cx).value().to_string();
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => {
                    this.submit(cx);
                }
                _ => {}
            }
        })
        .detach();
        Self {
            state,
            ingest_input,
            pending_text: String::new(),
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let text = self.pending_text.trim();
        if text.is_empty() {
            return;
        }
        let req = MemoryIngestRequest {
            text: Some(text.to_string()),
            source: Some(INGEST_SOURCE.to_string()),
            ..Default::default()
        };
        // Fire-and-forget — `submit_ingest` spawns its own thread; the
        // result lands back through the worker channel as
        // `AppEvent::MemoryIngestResult` (+ a follow-up
        // `MemoryRefresh` on success). Input text is intentionally
        // NOT cleared here — `InputState::set_value` needs a Window
        // reference we don't have inside the click handler. The user
        // can backspace if they want a fresh slate.
        self.state.read(cx).submit_ingest(req);
        cx.notify();
    }
}

impl Render for KnowledgeRoute {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let secondary = cx.theme().secondary;
        let primary = cx.theme().primary;
        let success = cx.theme().success;
        let danger = cx.theme().danger;

        let status_line: gpui::AnyElement = match state.last_ingest.as_ref() {
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

        let submit_disabled = self.pending_text.trim().is_empty();
        let submit_color = if submit_disabled { muted } else { primary };
        // Capture an entity handle for the click handler to read +
        // call `submit_ingest` through. Cloning is cheap.
        let route_entity = cx.entity();
        let submit_btn = div()
            .id("memory-ingest-submit")
            .px_3()
            .py_1()
            .border_1()
            .border_color(submit_color)
            .rounded_md()
            .text_color(submit_color)
            .child(SharedString::new_static("Ingest"))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                route_entity.update(cx, |this, cx| this.submit(cx));
            });

        let form = h_flex()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .border_1()
                    .border_color(border)
                    .rounded_md()
                    .px_2()
                    .py_1()
                    .child(Input::new(&self.ingest_input).appearance(false)),
            )
            .child(submit_btn);

        let header = v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_3()
                    .child(div().text_lg().child(SharedString::new_static("Knowledge")))
                    .child(div().text_color(muted).child(SharedString::new_static(
                        "Drawers refresh every 10s · Enter or click Ingest to submit.",
                    ))),
            )
            .child(form)
            .child(status_line);

        let body: gpui::AnyElement = match state.memory_recent.as_ref() {
            None => div()
                .text_color(muted)
                .min_h(px(60.0))
                .child(SharedString::new_static("loading drawers…"))
                .into_any_element(),
            Some(resp) if !resp.present => div()
                .text_color(muted)
                .min_h(px(60.0))
                .child(SharedString::new_static(
                    "memory backend not initialised — run `crabcc memory init` \
                     to create `.crabcc/memory.db` for this repo.",
                ))
                .into_any_element(),
            Some(resp) if resp.drawers.is_empty() => div()
                .text_color(muted)
                .min_h(px(60.0))
                .child(SharedString::new_static(
                    "no drawers yet — `crabcc memory ingest` from the CLI \
                     adds new ones.",
                ))
                .into_any_element(),
            Some(resp) => {
                let count_line = SharedString::from(format!(
                    "{} drawers · cursor {}",
                    resp.drawers.len(),
                    resp.cursor
                ));
                v_flex()
                    .gap_2()
                    .child(div().text_color(muted).child(count_line))
                    .child(
                        v_flex().gap_1().children(
                            resp.drawers
                                .iter()
                                .take(VISIBLE_DRAWERS)
                                .map(|d| drawer_row(d, muted, border, secondary).into_any_element())
                                .collect::<Vec<_>>(),
                        ),
                    )
                    .into_any_element()
            }
        };

        v_flex()
            .size_full()
            .px_5()
            .py_4()
            .gap_3()
            .child(header)
            .child(body)
    }
}

fn drawer_row(d: &MemoryDrawer, muted: Hsla, border: Hsla, badge_bg: Hsla) -> gpui::Div {
    let location = match d.room.as_deref() {
        Some(room) if !room.is_empty() => format!("{}/{}", d.wing, room),
        _ => d.wing.clone(),
    };

    v_flex()
        .gap_1()
        .py_2()
        .border_b_1()
        .border_color(border)
        .child(
            h_flex()
                .gap_3()
                // Drawer id — fixed-width column for visual alignment.
                .child(
                    div()
                        .w(px(60.0))
                        .text_color(muted)
                        .child(SharedString::from(format!("#{}", d.id))),
                )
                // Wing/room badge — uses the secondary token as a
                // subtle pill background.
                .child(
                    div()
                        .px_2()
                        .py_0p5()
                        .bg(badge_bg)
                        .rounded_md()
                        .child(SharedString::from(location)),
                )
                .child(
                    div()
                        .text_color(muted)
                        .child(SharedString::from(format_relative(d.created_at))),
                ),
        )
        .child(SharedString::from(truncate(&d.body_preview, 220)))
}

/// "Ns ago" / "Nm ago" / "Nh ago" — coarse but readable for a
/// developer-facing memory tail. Computed against
/// `SystemTime::now()` so timezone is irrelevant.
fn format_relative(unix_seconds: i64) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(unix_seconds);
    let delta = (now - unix_seconds).max(0);
    if delta < 60 {
        format!("{delta}s ago")
    } else if delta < 3600 {
        format!("{}m ago", delta / 60)
    } else if delta < 86_400 {
        format!("{}h ago", delta / 3600)
    } else {
        format!("{}d ago", delta / 86_400)
    }
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
    fn relative_buckets() {
        // Computed against `now`, so we exercise the relative bucket
        // ladder by passing relative offsets.
        let now: i64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!(format_relative(now - 5).ends_with("s ago"));
        assert!(format_relative(now - 120).ends_with("m ago"));
        assert!(format_relative(now - 7200).ends_with("h ago"));
        assert!(format_relative(now - 200_000).ends_with("d ago"));
    }

    #[test]
    fn truncate_appends_ellipsis() {
        assert_eq!(truncate("abc", 10), "abc");
        assert_eq!(truncate("abcdef", 4), "abc…");
    }
}
