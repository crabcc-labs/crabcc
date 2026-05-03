//! In-window toast strip — track C.0.
//!
//! Pure-GPUI surface that renders up to [`MAX_VISIBLE_TOASTS`]
//! stacked toasts above whatever route is active. Independent of
//! the macOS rich-notification stack (track C.2+). Ships before the
//! native side so error / status surfacing has a home from day one.
//!
//! ## What's wired today (slice 2)
//!
//! - Data model + render component (slice 1, [#308]).
//! - Per-toast manual dismiss via the `×` button.
//! - Auto-dismiss via [`Toast::is_active`] — render-time skip plus
//!   in-state GC ([`AppState::gc_expired_toasts`]) called from
//!   every `push_toast`. Persistent levels (warning / danger) stay
//!   until manually dismissed.
//! - Auto-emit on six result events: `MemoryIngestResult`,
//!   `AgentLaunchResult`, `AgentKillResult` × `Ok` / `Err`. Level
//!   selection: success → Success(5s) / launch ok → Primary(8s) /
//!   kill ok → Info(3s) / errors → Danger (persist).
//!
//! ## What's intentionally not here
//!
//! - **`↗ system` echo-dedup tag** — track C.2 once the AppKit
//!   rich-notification side exists.
//! - **Mute toggle + Settings entrypoint + `Show last 50 →` log**
//!   — later slices.
//!
//! [#308]: https://github.com/peterlodri-sec/crabcc/pull/308

use gpui::{div, prelude::*, px, Context, Entity, MouseButton, Render, SharedString, Window};
use gpui_component::{h_flex, v_flex, ActiveTheme};

use crate::state::AppState;

/// Toast level — selects accent colour and (eventually) the
/// auto-dismiss interval. Order mirrors the design brief's badge
/// stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastLevel {
    Success,
    Info,
    Warning,
    Danger,
    Primary,
}

impl ToastLevel {
    /// Auto-dismiss interval in seconds. `None` means "persist
    /// until the user dismisses manually" — used for warning
    /// (operator should ack) and danger (must ack).
    pub fn dismiss_after_secs(self) -> Option<u64> {
        match self {
            ToastLevel::Success => Some(5),
            ToastLevel::Info => Some(3),
            ToastLevel::Warning => None,
            ToastLevel::Danger => None,
            ToastLevel::Primary => Some(8),
        }
    }

    /// Glyph rendered to the left of the message — gives the row a
    /// quick at-a-glance identifier even when the user can't see the
    /// accent colour.
    pub fn glyph(self) -> &'static str {
        match self {
            ToastLevel::Success => "\u{2713}", // ✓
            ToastLevel::Info => "\u{2139}",    // ℹ
            ToastLevel::Warning => "!",
            ToastLevel::Danger => "\u{2717}",  // ✗
            ToastLevel::Primary => "\u{2605}", // ★
        }
    }
}

/// One stacked notification row.
///
/// `id` is monotonic per-`AppState` so the dismiss-button click can
/// target a specific row even after the deque has been GC'd.
/// `created_at` is the wall-clock the toast was pushed; consumed
/// by [`Toast::is_active`] for render-time auto-dismiss.
#[derive(Debug, Clone)]
pub struct Toast {
    pub id: u64,
    pub level: ToastLevel,
    pub message: SharedString,
    pub created_at: i64,
}

impl Toast {
    /// Whether the toast is still inside its auto-dismiss window
    /// at the given wall-clock `now`. Persistent levels (warning /
    /// danger) always return `true` — they require a manual
    /// dismiss. Used by both the in-state GC
    /// (`AppState::gc_expired_toasts`) and the render-time
    /// auto-dismiss filter.
    pub fn is_active(&self, now: i64) -> bool {
        match self.level.dismiss_after_secs() {
            None => true,
            Some(secs) => (now - self.created_at) < secs as i64,
        }
    }
}

/// Maximum simultaneously-visible toasts. Excess oldest-first
/// rows are dropped at push time so the strip never grows past
/// this height. Picked by the design brief.
pub const MAX_VISIBLE_TOASTS: usize = 5;

/// View entity for the strip. Reads from `AppState::toasts` and
/// re-renders on `cx.notify()` (observed in `new`).
pub struct ToastStrip {
    state: Entity<AppState>,
}

impl ToastStrip {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self { state }
    }
}

impl Render for ToastStrip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        // Render-time auto-dismiss filter: hide toasts whose
        // dismiss interval has lapsed even if `gc_expired_toasts`
        // hasn't run since (e.g. no other event has fired
        // `push_toast` to trigger a GC). The deque-side GC is the
        // primary path; this just guarantees no expired toast is
        // visible past its second.
        let now = state.last_event_ts.unwrap_or(0);
        let any_visible = state.toasts.iter().any(|t| t.is_active(now));
        if !any_visible {
            // Render nothing when empty / all expired — keeps the
            // strip from taking layout space.
            return div();
        }
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let bg = theme.secondary;
        let state_for_dismiss = self.state.clone();

        div().child(
            v_flex().px_5().py_2().gap_2().children(
                state
                    .toasts
                    .iter()
                    .filter(|t| t.is_active(now))
                    .take(MAX_VISIBLE_TOASTS)
                    .map(|t| {
                        let accent = match t.level {
                            ToastLevel::Success => theme.success,
                            ToastLevel::Info => theme.info,
                            ToastLevel::Warning => theme.warning,
                            ToastLevel::Danger => theme.danger,
                            ToastLevel::Primary => theme.primary,
                        };
                        let id_for_click = t.id;
                        let state_clone = state_for_dismiss.clone();
                        // Element id must be unique per render pass; the
                        // monotonic toast id makes that trivial.
                        let dismiss_id: gpui::ElementId =
                            SharedString::from(format!("toast-dismiss-{}", t.id)).into();
                        h_flex()
                            .gap_3()
                            .px_3()
                            .py_2()
                            .border_1()
                            .border_color(accent)
                            .rounded_md()
                            .bg(bg)
                            .child(
                                div()
                                    .w(px(16.0))
                                    .text_color(accent)
                                    .child(SharedString::new_static(t.level.glyph())),
                            )
                            .child(div().flex_1().child(t.message.clone()))
                            .child(
                                div()
                                    .id(dismiss_id)
                                    .px_2()
                                    .text_color(muted)
                                    .child(SharedString::new_static("\u{00D7}"))
                                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                        state_clone.update(cx, |s, cx| {
                                            s.dismiss_toast(id_for_click);
                                            cx.notify();
                                        });
                                    }),
                            )
                            .into_any_element()
                    }),
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dismiss_intervals_match_brief() {
        // Brief: success 5s, info 3s, warning persists, danger
        // persists, primary 8s. Pin those — drift is detectable
        // here without firing up a window.
        assert_eq!(ToastLevel::Success.dismiss_after_secs(), Some(5));
        assert_eq!(ToastLevel::Info.dismiss_after_secs(), Some(3));
        assert_eq!(ToastLevel::Warning.dismiss_after_secs(), None);
        assert_eq!(ToastLevel::Danger.dismiss_after_secs(), None);
        assert_eq!(ToastLevel::Primary.dismiss_after_secs(), Some(8));
    }

    #[test]
    fn glyphs_are_unique_per_level() {
        // Glyphs are the at-a-glance discriminator when colour
        // isn't enough — make sure no two levels share one.
        let glyphs = [
            ToastLevel::Success.glyph(),
            ToastLevel::Info.glyph(),
            ToastLevel::Warning.glyph(),
            ToastLevel::Danger.glyph(),
            ToastLevel::Primary.glyph(),
        ];
        let original_len = glyphs.len();
        let mut sorted: Vec<&str> = glyphs.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            original_len,
            "duplicate level glyphs: {glyphs:?}"
        );
    }
}
