//! Shared empty-state block for route bodies.
//!
//! Centered glyph + headline + hint, sized so it reads as a
//! deliberate empty state rather than a layout glitch the user
//! has to second-guess. Promoted from inline use in
//! `routes::agents` once a second route (logs) wanted the same
//! shape.

use gpui::{div, prelude::*, Div, Hsla, SharedString};

/// Build a centered empty-state element.
///
/// `glyph` is a single static unicode codepoint (e.g. `"\u{25CC}"`
/// or `"\u{1F50D}"`). `headline` is a one-line static string.
/// `hint` is a possibly-formatted runtime string (callers can
/// `format!` in a query into it).
pub fn empty_state(
    glyph: &'static str,
    headline: &'static str,
    hint: &str,
    muted: Hsla,
    foreground: Hsla,
) -> Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .px_5()
        .py_8()
        .child(
            div()
                .text_2xl()
                .text_color(muted)
                .child(SharedString::new_static(glyph)),
        )
        .child(
            div()
                .text_color(foreground)
                .child(SharedString::new_static(headline)),
        )
        .child(
            div()
                .text_xs()
                .text_color(muted)
                .child(SharedString::from(hint.to_string())),
        )
}
