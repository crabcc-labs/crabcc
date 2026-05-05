//! About modal — app description + version + dependency rollup.
//!
//! Rendered as an overlay layered on top of the Shell when
//! `is_open` is true. Triggered from the [`crate::settings::SettingsPanel`]
//! "About crabcc-desktop ›" link. Click outside the modal body
//! (or the `×` button) to close.
//!
//! Dependency list is hand-curated from `Cargo.toml` — auto-
//! generation from `cargo metadata` would balloon the body with
//! transitive crates and add a runtime fetch. The sourced list
//! covers the architecturally meaningful ones: gpui ecosystem,
//! HTTP / serialisation, cyberpunk-colour cli helpers, and the
//! native macOS bindings.

use gpui::{div, prelude::*, px, Context, IntoElement, MouseButton, Render, SharedString, Window};
use gpui_component::{h_flex, tooltip::Tooltip, v_flex, ActiveTheme};

/// One curated dependency to surface in the modal.
struct DepEntry {
    name: &'static str,
    role: &'static str,
}

/// Headline blurb shown at the top of the modal. Pulled from
/// `Cargo.toml`'s `description` field at build time so the two
/// stay in sync.
const APP_DESCRIPTION: &str = env!("CARGO_PKG_DESCRIPTION");

/// Curated dep list. Keep ordered by architectural layer so the
/// reader can scan the stack top-to-bottom.
const DEPS: &[DepEntry] = &[
    DepEntry {
        name: "gpui",
        role: "renderer + event loop (Zed)",
    },
    DepEntry {
        name: "gpui-component",
        role: "themed UI primitives (longbridge)",
    },
    DepEntry {
        name: "reqwest",
        role: "blocking HTTP client",
    },
    DepEntry {
        name: "serde + serde_json",
        role: "wire-type (de)serialisation",
    },
    DepEntry {
        name: "flume",
        role: "bounded MPSC for SSE / worker bridge",
    },
    DepEntry {
        name: "rayon",
        role: "force-directed graph layout",
    },
    DepEntry {
        name: "tracing + tracing-subscriber",
        role: "structured logging",
    },
    DepEntry {
        name: "ctrlc",
        role: "graceful shutdown signal handler",
    },
    DepEntry {
        name: "objc2 + objc2-app-kit + objc2-foundation",
        role: "native macOS surfaces (dock badge, status item)",
    },
    DepEntry {
        name: "anyhow",
        role: "ergonomic error handling",
    },
];

pub struct AboutModal {
    is_open: bool,
}

impl AboutModal {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self { is_open: false }
    }

    pub fn is_open(&self) -> bool {
        self.is_open
    }

    pub fn open(&mut self) {
        self.is_open = true;
    }

    pub fn close(&mut self) {
        self.is_open = false;
    }
}

impl Render for AboutModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.is_open {
            return div().into_any_element();
        }

        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let primary = theme.primary;
        let border = theme.border;
        let bg = theme.secondary;
        // `theme.background` (main app surface) is darker than the
        // modal's `secondary` bg, so it reads as a "depressed" hover
        // tint inside the modal — same convention as the settings
        // panel rows in #393.
        let hover_bg = theme.background;
        let entity_self = cx.entity();

        let title = h_flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_lg()
                    .text_color(theme.foreground)
                    .child(SharedString::new_static("crabcc · live")),
            )
            .child({
                let entity_for_close = entity_self.clone();
                div()
                    .id("about-close")
                    .px_2()
                    .py_0p5()
                    .text_color(muted)
                    .border_1()
                    .border_color(border)
                    .rounded_md()
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover_bg))
                    .tooltip(|window, cx| Tooltip::new("Close About").build(window, cx))
                    .child(SharedString::new_static("\u{00D7}"))
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        entity_for_close.update(cx, |this, cx| {
                            this.close();
                            cx.notify();
                        });
                    })
            });

        let version_line = div().text_color(muted).child(SharedString::from(format!(
            "v{} · {}",
            env!("CARGO_PKG_VERSION"),
            env!("CARGO_PKG_REPOSITORY"),
        )));

        let description = div()
            .text_color(theme.foreground)
            .child(SharedString::new_static(APP_DESCRIPTION));

        let deps_title = div()
            .text_xs()
            .text_color(muted)
            .child(SharedString::new_static("BUILT WITH"));

        let deps_list = v_flex().gap_1().children(DEPS.iter().map(|d| {
            h_flex()
                .gap_2()
                .child(
                    div()
                        .text_color(primary)
                        .min_w(px(220.0))
                        .child(SharedString::new_static(d.name)),
                )
                .child(
                    div()
                        .text_color(muted)
                        .child(SharedString::new_static(d.role)),
                )
                .into_any_element()
        }));

        let deps_section = v_flex().gap_2().child(deps_title).child(deps_list);

        // License + copyright line. Year is hard-coded — bump on
        // each release. Author/owner is fetched from CARGO_PKG_AUTHORS,
        // which is `peterlodri-sec` per the workspace Cargo.toml. When
        // co-author lines land, this becomes a Vec.
        let license_line = div()
            .text_xs()
            .text_color(muted)
            .child(SharedString::new_static(
                "MIT licensed · © 2026 peterlodri-sec",
            ));

        // Modal body — Stateful so the click handler can call
        // `cx.stop_propagation()` and prevent the click from bubbling
        // into the backdrop's "close on click" handler. Without this,
        // clicking anywhere inside the modal (e.g. to select text in
        // the dependency list) would dismiss the modal — surprising.
        let modal_body = v_flex()
            .id("about-modal-body")
            .px_5()
            .py_4()
            .gap_4()
            .bg(bg)
            .border_1()
            .border_color(border)
            .rounded_lg()
            .max_w(px(640.0))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .child(title)
            .child(version_line)
            .child(description)
            .child(deps_section)
            .child(license_line);

        // Backdrop dims the rest of the window. Click outside the
        // modal body closes — same gesture as the existing
        // ReindexDialog (web). `cursor_pointer` telegraphs that the
        // backdrop is interactive (an unlabelled dim region wouldn't
        // otherwise read as clickable).
        let entity_for_backdrop = entity_self.clone();
        div()
            .id("about-modal-backdrop")
            .absolute()
            .inset_0()
            .bg(gpui::black().opacity(0.5))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .tooltip(|window, cx| Tooltip::new("Click backdrop to close About").build(window, cx))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                entity_for_backdrop.update(cx, |this, cx| {
                    this.close();
                    cx.notify();
                });
            })
            .child(modal_body)
            .into_any_element()
    }
}
