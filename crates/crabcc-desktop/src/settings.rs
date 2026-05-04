//! Settings panel — inline picker for theme + future preferences.
//!
//! Rendered between the toast strip and the body slot in `Shell`,
//! same lifetime model as `ToastStrip`. Renders nothing when
//! [`SettingsPanel::is_open`] is `false` so the layout stays
//! unchanged for the common case.
//!
//! Today's surface: a 5-row palette picker. Each row shows the
//! palette name and a primary-coloured tick on the active one.
//! Click any row to apply (mutate the global theme + persist via
//! [`crate::theme::save_persisted_palette`]) + close the panel.
//!
//! Adding more settings (alerts mute / echo / `STOP_SERVICES_ON_EXIT`
//! mirror) is a matter of dropping new sections into the render
//! body — the open/close model is reusable.

use gpui::{
    div, prelude::*, px, Context, Entity, IntoElement, MouseButton, Render, SharedString, Window,
};
use gpui_component::{h_flex, v_flex, ActiveTheme};

use crate::state::AppState;
use crate::theme::Palette;

pub struct SettingsPanel {
    state: Entity<AppState>,
    /// Open/close gate. Toggled by the header gear button via
    /// `Shell::render`. Initial value: closed (settings is an
    /// occasional surface, not always-on).
    is_open: bool,
}

impl SettingsPanel {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self {
            state,
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
        let active_idx = self.state.read(cx).palette_index;
        let entity_self = cx.entity();

        // ── Theme section ──────────────────────────────────────
        let title = div()
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
            .child(title)
            .child(v_flex().gap_1().children(palette_rows));

        div()
            .child(
                v_flex()
                    .px_5()
                    .py_3()
                    .gap_3()
                    .bg(bg)
                    .border_1()
                    .border_color(border)
                    .border_t_1()
                    .child(theme_section),
            )
            .min_w(px(280.0))
    }
}
