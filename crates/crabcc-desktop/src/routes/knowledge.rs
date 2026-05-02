//! Knowledge route — memory drawer browser.
//!
//! Reads from `AppState::memory_recent`, populated once by the prefetch
//! worker (no SSE topic for memory yet). Renders newest-first with the
//! drawer's wing/room badge, creation timestamp, and body preview.
//!
//! Out of scope this round: ingest box, drawer-detail view, knowledge
//! graph. The CLI (`crabcc memory ingest`) and the React `IngestBox`
//! remain the canonical entry points until those land.

use gpui::{
    div, prelude::*, px, Context, Entity, Hsla, IntoElement, Render, SharedString, Window,
};
use gpui_component::{h_flex, v_flex, ActiveTheme};

use crate::api::types::MemoryDrawer;
use crate::state::AppState;

const VISIBLE_DRAWERS: usize = 50;

pub struct KnowledgeRoute {
    state: Entity<AppState>,
}

impl KnowledgeRoute {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self { state }
    }
}

impl Render for KnowledgeRoute {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let secondary = cx.theme().secondary;

        let header = h_flex().gap_3().child(
            div()
                .text_lg()
                .child(SharedString::new_static("Knowledge")),
        );

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
