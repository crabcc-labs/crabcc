//! In-window toast strip — track C.0.
//!
//! Pure-GPUI surface that renders up to [`MAX_VISIBLE_TOASTS`]
//! stacked toasts above whatever route is active. Independent of
//! the macOS rich-notification stack (track C.2+). Ships before the
//! native side so error / status surfacing has a home from day one.
//!
//! ## What's wired today
//!
//! - Data model + render component (slice 1).
//! - Per-toast manual dismiss via the `×` button.
//! - Auto-dismiss via [`Toast::is_active`] — render-time skip plus
//!   in-state GC ([`AppState::gc_expired_toasts`]) called from
//!   every `push_toast`. Persistent levels (warning / danger) stay
//!   until manually dismissed (slice 2).
//! - Auto-emit on six result events: `MemoryIngestResult`,
//!   `AgentLaunchResult`, `AgentKillResult` × `Ok` / `Err`
//!   (slice 2).
//! - Edge-trigger emit on prefetch + telemetry/memory poll
//!   failures and recoveries (slice 3).
//! - Header mute toggle + `AppState::toasts_muted` — `push_toast`
//!   skips the enqueue when muted but still hands out unique ids
//!   so edge-trigger sentinels keep working (slice 4).
//! - Append-only `AppState::toast_history` log (cap 50) recording
//!   every push, including muted ones; footer "history (N) ·
//!   clear" row (slice 5).
//!
//! ## What's intentionally not here
//!
//! - **Dedicated history view** — clicking the count is a no-op
//!   today; a route stub or expanded view lands in a later slice.
//! - **`↗ system` echo-dedup tag** — track C.2 once the AppKit
//!   rich-notification side exists.
//! - **"Settings" entrypoint** — later slice.

use gpui::{
    div, prelude::*, px, relative, Context, Entity, MouseButton, Render, SharedString, Window,
};
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
    /// Whether the audit log is currently expanded inline below
    /// the active strip. Toggled by clicking the footer's
    /// "expand" / "collapse" affordance — pure UI state, not
    /// persisted across app restarts.
    expanded: bool,
}

impl ToastStrip {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self {
            state,
            expanded: false,
        }
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
        let history_len = state.toast_history.len();
        if !any_visible && history_len == 0 {
            // Empty active deque AND empty history — nothing to
            // show, claim zero layout. The footer keeps the strip
            // visible whenever the operator has anything to audit.
            return div();
        }
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let bg = theme.secondary;
        let state_for_dismiss = self.state.clone();
        let state_for_clear = self.state.clone();
        let state_for_dismiss_all = self.state.clone();
        // Snapshot the echo flag so the per-toast row closure can
        // decide whether to render the `↗ system` tag without
        // re-reading the state entity inside the closure.
        let echo_to_system = state.echo_to_system;
        let active_visible = state.toasts.iter().filter(|t| t.is_active(now)).count();

        // Footer "[dismiss all] · history (N) · expand · clear" row.
        // `dismiss all` only renders when 2+ toasts are visible — a
        // single toast already has its own `×` so the convenience
        // doesn't earn its layout. `history (N) · expand · clear`
        // only renders when the audit log is non-empty.
        let expanded = self.expanded;
        let entity_for_expand = cx.entity();
        let mut footer_row = h_flex().gap_2().px_5().pb_1();
        if active_visible >= 2 {
            footer_row = footer_row.child(
                div()
                    .id("toast-dismiss-all")
                    .text_color(muted)
                    .child(SharedString::new_static("dismiss all"))
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        state_for_dismiss_all.update(cx, |s, cx| {
                            s.clear_visible_toasts();
                            cx.notify();
                        });
                    }),
            );
            if history_len > 0 {
                footer_row =
                    footer_row.child(div().text_color(muted).child(SharedString::new_static("·")));
            }
        }
        let footer: gpui::AnyElement = if history_len > 0 {
            let count_label = SharedString::from(format!("history ({history_len})"));
            let expand_label = if expanded { "collapse" } else { "expand" };
            footer_row
                .child(div().text_color(muted).child(count_label))
                .child(div().text_color(muted).child(SharedString::new_static("·")))
                .child(
                    div()
                        .id("toast-history-expand")
                        .text_color(muted)
                        .child(SharedString::new_static(expand_label))
                        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                            entity_for_expand.update(cx, |this, cx| {
                                this.expanded = !this.expanded;
                                cx.notify();
                            });
                        }),
                )
                .child(div().text_color(muted).child(SharedString::new_static("·")))
                .child(
                    div()
                        .id("toast-history-clear")
                        .text_color(muted)
                        .child(SharedString::new_static("clear"))
                        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                            state_for_clear.update(cx, |s, cx| {
                                s.clear_toast_history();
                                cx.notify();
                            });
                        }),
                )
                .into_any_element()
        } else if active_visible >= 2 {
            footer_row.into_any_element()
        } else {
            div().into_any_element()
        };

        // Expanded audit list. Newest-first (mirrors the active
        // strip), single-line per entry. No dismiss button —
        // history is a log, not active surface. Renders only when
        // toggled open and history is non-empty.
        let history_panel: gpui::AnyElement = if expanded && history_len > 0 {
            v_flex()
                .px_5()
                .py_1()
                .gap_1()
                .border_t_1()
                .border_color(theme.border)
                .children(state.toast_history.iter().rev().map(|t| {
                    let accent = match t.level {
                        ToastLevel::Success => theme.success,
                        ToastLevel::Info => theme.info,
                        ToastLevel::Warning => theme.warning,
                        ToastLevel::Danger => theme.danger,
                        ToastLevel::Primary => theme.primary,
                    };
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .w(px(16.0))
                                .text_color(accent)
                                .child(SharedString::new_static(t.level.glyph())),
                        )
                        .child(div().flex_1().text_color(muted).child(t.message.clone()))
                        .into_any_element()
                }))
                .into_any_element()
        } else {
            div().into_any_element()
        };

        v_flex()
            .child(
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
                            let state_for_box = state_for_dismiss.clone();
                            // Element ids must be unique per render pass; the
                            // monotonic toast id makes that trivial.
                            // `NamedInteger` keeps the per-render allocation
                            // at zero — the static-backed name pairs with
                            // the toast id directly, no `format!()` /
                            // `SharedString::from(String)` round-trip.
                            let dismiss_id = gpui::ElementId::NamedInteger(
                                SharedString::new_static("toast-dismiss"),
                                t.id,
                            );
                            let box_id = gpui::ElementId::NamedInteger(
                                SharedString::new_static("toast-box"),
                                t.id,
                            );
                            // Remaining-time fraction for the progress
                            // bar (Some(_) for transient levels with a
                            // dismiss-after, None for warning/danger
                            // which persist until manually acked).
                            // Goes 1.0 → 0.0 as the toast lives, so
                            // the visible bar shrinks toward dismiss.
                            // Render-only; the actual GC still runs
                            // in `is_active`.
                            let progress: Option<f32> = t.level.dismiss_after_secs().map(|secs| {
                                let elapsed = (now - t.created_at).max(0) as f32;
                                let total = secs as f32;
                                ((total - elapsed) / total).clamp(0.0, 1.0)
                            });

                            let row = h_flex()
                                .gap_3()
                                .px_3()
                                .py_2()
                                .child(
                                    div()
                                        .w(px(16.0))
                                        .text_color(accent)
                                        .child(SharedString::new_static(t.level.glyph())),
                                )
                                .child(div().flex_1().child(t.message.clone()))
                                // "↗ system" tag — visible only when
                                // `AppState::echo_to_system` is on, in
                                // which case every visible toast is
                                // also delivered to Notification Center
                                // via `Shell::render` →
                                // `native::deliver_notification`. When
                                // echo is off, the toast still shows
                                // in-window but no banner fires; the
                                // tag would be a lie.
                                .child(if echo_to_system {
                                    div()
                                        .px_1()
                                        .text_color(muted)
                                        .child(SharedString::new_static(
                                            "\u{2197} system",
                                        ))
                                } else {
                                    div()
                                })
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
                                );

                            // Progress bar — 2px track at the bottom
                            // of the toast box, with the accent fill
                            // shrinking left-to-right as time runs
                            // out. Persistent levels (warning /
                            // danger) render an empty placeholder so
                            // the toast's visual height stays
                            // consistent with transient toasts above
                            // / below.
                            let bar: gpui::AnyElement = match progress {
                                Some(f) => div()
                                    .h(px(2.0))
                                    .w_full()
                                    .bg(theme.border)
                                    .child(div().h_full().w(relative(f)).bg(accent))
                                    .into_any_element(),
                                None => div().h(px(2.0)).into_any_element(),
                            };

                            // Whole toast box becomes a click target —
                            // dismisses on click anywhere on the row,
                            // not just the small × glyph. The × stays
                            // in the row for discoverability; both
                            // handlers fire `dismiss_toast`, which is
                            // idempotent (the second fires through a
                            // no-op `retain` since the toast is gone).
                            v_flex()
                                .id(box_id)
                                .border_1()
                                .border_color(accent)
                                .rounded_md()
                                .bg(bg)
                                .overflow_hidden()
                                .cursor_pointer()
                                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                    state_for_box.update(cx, |s, cx| {
                                        s.dismiss_toast(id_for_click);
                                        cx.notify();
                                    });
                                })
                                .child(row)
                                .child(bar)
                                .into_any_element()
                        }),
                ),
            )
            .child(footer)
            .child(history_panel)
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
