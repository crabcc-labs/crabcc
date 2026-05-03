use gpui::{
    Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled, Window,
    div, h_flex, v_flex,
};

use crate::home::DashboardHome;
use crate::native;
use crate::state::{AppState, Route};

/// Top-level application shell rendered by GPUI.
///
/// # Performance notes
///
/// `render` can fire up to 120 times per second during animations, scrolling,
/// or window resizing.  Two classes of unnecessary work have been eliminated:
///
/// 1. **Dock badge guard** — `last_badge_count` ensures `native::set_dock_badge`
///    only crosses the AppKit boundary when the running-agent count *changes*.
///
/// 2. **Cached brand string** — `cached_brand` holds the formatted
///    `"v{version}  {repo}"` string as a `SharedString` (backed by `Arc<str>`).
///    It is computed exactly once when `AppState::bootstrap` transitions from
///    `None` to `Some`.  On every subsequent frame the render loop only clones
///    an `Arc` ref-count — zero heap allocation.  Static fallback strings use
///    `SharedString::new_static`, which stores a `&'static str` pointer with
///    no allocation at all.
pub struct Shell {
    state: Entity<AppState>,
    home: Entity<DashboardHome>,
    /// Sentinel: `u32::MAX` until the first real update; guards against
    /// redundant AppKit calls when the running count hasn't actually changed.
    last_badge_count: u32,

    /// Cached `"v{version}  {repo}"` label. Populated lazily on the first
    /// frame where bootstrap data is present; `None` while still loading.
    cached_brand: Option<SharedString>,
}

impl Shell {
    pub fn new(
        state: Entity<AppState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let home = cx.new(|cx| DashboardHome::new(window, cx));
        Self {
            state,
            home,
            // u32::MAX is a sentinel that guarantees the first real value
            // always triggers a badge update, even if running == 0.
            last_badge_count: u32::MAX,
            cached_brand: None,
        }
    }
}

impl Render for Shell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bg = cx.theme().background;
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let foreground = cx.theme().foreground;
        let primary = cx.theme().primary;

        let state_for_brand = self.state.read(cx);

        // --- Cached brand string -------------------------------------------
        // Lazily compute once when bootstrap data arrives.  All subsequent
        // frames just bump an Arc ref-count via `.clone()`.
        if self.cached_brand.is_none() {
            if let Some(b) = &state_for_brand.bootstrap {
                self.cached_brand = Some(SharedString::from(format!(
                    "v{}  {}",
                    b.version, b.repo
                )));
            }
        }
        // Zero-allocation path: cloning SharedString is an Arc ref-count bump.
        // Zero-allocation fallback: new_static stores a &'static str pointer.
        let brand_sub = self
            .cached_brand
            .clone()
            .unwrap_or_else(|| SharedString::new_static("loading…"));

        let active = state_for_brand.route;

        // --- Dock badge guard -----------------------------------------------
        // AppKit calls are expensive; only cross the boundary on change.
        let running = state_for_brand.agents_running();
        if running != self.last_badge_count {
            let label = (running > 0).then(|| running.to_string());
            native::set_dock_badge(label.as_deref());
            self.last_badge_count = running;
        }

        // --- Navigation bar -------------------------------------------------
        let nav = h_flex()
            .gap_2()
            .child(nav_item(
                SharedString::new_static("Home"),
                active == Route::Home,
                primary,
                foreground,
                muted,
            ))
            .child(nav_item(
                SharedString::new_static("Agents"),
                active == Route::Agents,
                primary,
                foreground,
                muted,
            ))
            .child(nav_item(
                SharedString::new_static("Settings"),
                active == Route::Settings,
                primary,
                foreground,
                muted,
            ));

        // --- Header ---------------------------------------------------------
        let header = h_flex()
            .w_full()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(border)
            .justify_between()
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .text_lg()
                            .text_color(foreground)
                            // Static string literal → zero allocation via new_static.
                            .child(SharedString::new_static("crabcc · live")),
                    )
                    // Pass the cached / zero-copy SharedString directly.
                    .child(div().text_color(muted).child(brand_sub)),
            )
            .child(nav);

        // --- Body -----------------------------------------------------------
        let body = div()
            .flex_1()
            .overflow_hidden()
            .child(self.home.clone());

        v_flex()
            .size_full()
            .bg(bg)
            .child(header)
            .child(body)
    }
}

// ---------------------------------------------------------------------------
// Helper: a single navigation pill
// ---------------------------------------------------------------------------

fn nav_item(
    label: SharedString,
    active: bool,
    primary: gpui::Hsla,
    foreground: gpui::Hsla,
    muted: gpui::Hsla,
) -> impl IntoElement {
    let color = if active { primary } else { muted };
    div()
        .px_3()
        .py_1()
        .rounded_md()
        .text_color(if active { foreground } else { muted })
        .when(active, |el| el.bg(color.alpha(0.12)))
        .child(label)
}
