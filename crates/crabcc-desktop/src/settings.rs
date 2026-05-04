//! Settings panel — inline preferences (theme + alerts + about).
//!
//! Rendered between the toast strip and the body slot in `Shell`,
//! same lifetime model as `ToastStrip`. Renders nothing when
//! [`SettingsPanel::is_open`] is `false` so the layout stays
//! unchanged for the common case.
//!
//! Sections:
//!
//!   THEME       — 5-row palette picker (jumps directly to the
//!                 chosen palette, persists, closes the panel).
//!   ALERTS      — toggle rows for `toasts_muted` and
//!                 `echo_to_system`. Mirror the header buttons —
//!                 the panel is the "all preferences in one
//!                 place" surface, the header buttons are the
//!                 fast-path.
//!   ABOUT       — opens the [`crate::about::AboutModal`]
//!                 overlay.
//!
//! Adding more sections is straightforward — drop a new section
//! into [`SettingsPanel::render`].

use gpui::{
    div, prelude::*, px, Context, Entity, IntoElement, MouseButton, Render, SharedString, Window,
};
use gpui_component::{h_flex, v_flex, ActiveTheme};

use crate::about::AboutModal;
use crate::state::AppState;
use crate::theme::Palette;

pub struct SettingsPanel {
    state: Entity<AppState>,
    about: Entity<AboutModal>,
    /// Open/close gate. Toggled by the header gear button via
    /// `Shell::render`. Initial value: closed (settings is an
    /// occasional surface, not always-on).
    is_open: bool,
}

impl SettingsPanel {
    pub fn new(state: Entity<AppState>, about: Entity<AboutModal>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        // Re-render the panel when the about modal opens / closes
        // so any visual cues (e.g. a future "× about-open"
        // indicator) reflect the change.
        cx.observe(&about, |_, _, cx| cx.notify()).detach();
        Self {
            state,
            about,
            is_open: false,
        }
    }

    pub fn is_open(&self) -> bool {
        self.is_open
    }

    pub fn toggle(&mut self) {
        self.is_open = !self.is_open;
    }

    pub fn close(&mut self) {
        self.is_open = false;
    }
}

impl Render for SettingsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.is_open {
            return div();
        }

        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let primary = theme.primary;
        let bg = theme.secondary;
        let border = theme.border;
        let app_state = self.state.read(cx);
        let active_idx = app_state.palette_index;
        let muted_now = app_state.toasts_muted;
        let echo_now = app_state.echo_to_system;
        let entity_self = cx.entity();

        // ── Theme section ──────────────────────────────────────
        let theme_title = div()
            .text_xs()
            .text_color(muted)
            .child(SharedString::new_static("THEME"));

        let palette_rows = Palette::ALL_NAMES.iter().enumerate().map(|(idx, name)| {
            let is_active = idx == active_idx;
            let row_color = if is_active { primary } else { theme.foreground };
            let state_for_click = self.state.clone();
            let entity_for_close = entity_self.clone();
            // gpui requires per-element ids; the static name +
            // index pair makes them unique without per-render
            // alloc (matches the wrap-up perf wedge style).
            let row_id = gpui::ElementId::NamedInteger(
                SharedString::new_static("settings-palette-row"),
                idx as u64,
            );
            let label = SharedString::from(format!(
                "{} {}",
                if is_active { "\u{25C9}" } else { "\u{25CB}" },
                name,
            ));
            h_flex()
                .id(row_id)
                .gap_2()
                .py_1()
                .px_2()
                .rounded_md()
                .border_1()
                .border_color(if is_active { primary } else { border })
                .text_color(row_color)
                .child(label)
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    // Apply + persist + close. Same flow as the
                    // header cycle button — but jumps directly to
                    // the chosen index instead of incrementing.
                    state_for_click.update(cx, |s, cx| {
                        s.palette_index = idx;
                        cx.notify();
                    });
                    let palette = crate::theme::apply_by_index(cx, idx);
                    let _ = palette;
                    let name = state_for_click.read(cx).palette_name();
                    crate::theme::save_persisted_palette(name);
                    entity_for_close.update(cx, |this, cx| {
                        this.close();
                        cx.notify();
                    });
                    window.refresh();
                })
                .into_any_element()
        });

        let theme_section = v_flex()
            .gap_2()
            .child(theme_title)
            .child(v_flex().gap_1().children(palette_rows));

        // ── Alerts section ─────────────────────────────────────
        let alerts_title = div()
            .text_xs()
            .text_color(muted)
            .child(SharedString::new_static("ALERTS"));

        // Build a toggle row helper — both alerts settings share
        // the same shape (label + on/off glyph). Unlike palettes,
        // these are pure boolean toggles so we don't need a
        // multi-row picker.
        let mute_row = make_toggle_row(
            SharedString::new_static("settings-mute-toggle"),
            "mute alerts",
            muted_now,
            primary,
            theme.foreground,
            border,
            {
                let state = self.state.clone();
                move |cx: &mut gpui::App| {
                    state.update(cx, |s, cx| {
                        s.toggle_toast_mute();
                        cx.notify();
                    });
                }
            },
        );
        let echo_row = make_toggle_row(
            SharedString::new_static("settings-echo-toggle"),
            "echo to Notification Center",
            echo_now,
            primary,
            theme.foreground,
            border,
            {
                let state = self.state.clone();
                move |cx: &mut gpui::App| {
                    state.update(cx, |s, cx| {
                        s.toggle_echo_to_system();
                        cx.notify();
                    });
                }
            },
        );

        let alerts_section = v_flex()
            .gap_2()
            .child(alerts_title)
            .child(v_flex().gap_1().child(mute_row).child(echo_row));

        // ── About link ─────────────────────────────────────────
        let about_entity = self.about.clone();
        let about_link = div()
            .id("settings-about-link")
            .text_color(muted)
            .child(SharedString::from(format!(
                "About crabcc-desktop v{} \u{203A}",
                env!("CARGO_PKG_VERSION")
            )))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                about_entity.update(cx, |modal, cx| {
                    modal.open();
                    cx.notify();
                });
            });

        div()
            .child(
                v_flex()
                    .px_5()
                    .py_3()
                    .gap_4()
                    .bg(bg)
                    .border_1()
                    .border_color(border)
                    .border_t_1()
                    .child(theme_section)
                    .child(alerts_section)
                    .child(about_link),
            )
            .min_w(px(280.0))
    }
}

/// Build a toggle row (`◉ label` when on, `○ label` when off).
/// The on-state matches the header buttons' colour treatment so
/// the panel and the header read identically. `on_click` is
/// called with `&mut App` so the closure can call
/// `state.update(...)` directly.
fn make_toggle_row<F>(
    id: SharedString,
    label: &'static str,
    on: bool,
    primary: gpui::Hsla,
    fg: gpui::Hsla,
    border: gpui::Hsla,
    on_click: F,
) -> gpui::Stateful<gpui::Div>
where
    F: Fn(&mut gpui::App) + 'static,
{
    let glyph = if on { "\u{25C9}" } else { "\u{25CB}" };
    let row_color = if on { primary } else { fg };
    let row_border = if on { primary } else { border };
    let label_text = SharedString::from(format!("{glyph} {label}"));
    h_flex()
        .id(gpui::ElementId::Name(id))
        .gap_2()
        .py_1()
        .px_2()
        .rounded_md()
        .border_1()
        .border_color(row_border)
        .text_color(row_color)
        .child(label_text)
        .on_mouse_down(MouseButton::Left, move |_, _, cx| on_click(cx))
}
