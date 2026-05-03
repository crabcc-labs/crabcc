//! Top-level Shell view — header + nav + body slot.
//!
//! Owns:
//!   * the brand line ("crabcc · live") and the version / repo hint
//!   * the route-nav strip (Home / Logs / System / Knowledge)
//!   * one instance of each sub-route view, kept alive across switches
//!     so their internal state (graph layout cache, observers) doesn't
//!     reset when the user clicks back
//!
//! The visible body switches on `AppState::route` — no real router
//! library, just a `match` on the enum.

use gpui::{
    div, prelude::*, px, AnyElement, Context, Entity, IntoElement, MouseButton, Render,
    SharedString, Window,
};
use gpui_component::{h_flex, v_flex, ActiveTheme};

use crate::native;
use crate::routes::{
    agents::AgentsRoute, commands::CommandsRoute, dashboard::DashboardHome,
    knowledge::KnowledgeRoute, logs::LogsRoute, system::SystemRoute,
};
use crate::state::{AppState, Route};

pub struct Shell {
    state: Entity<AppState>,
    home: Entity<DashboardHome>,
    agents: Entity<AgentsRoute>,
    logs: Entity<LogsRoute>,
    system: Entity<SystemRoute>,
    knowledge: Entity<KnowledgeRoute>,
    commands: Entity<CommandsRoute>,
    /// Most-recent value passed to `native::set_dock_badge`, so the
    /// render path can skip the AppKit call when the count hasn't
    /// changed. `u32::MAX` is the sentinel "never set yet" — picked
    /// instead of `Option<u32>` so the comparison is a single integer
    /// equality check on every render.
    last_badge_count: u32,
    /// Cached `"v{version}  {repo}"` label. Populated lazily on the first
    /// render frame where `AppState::bootstrap` is `Some`; `None` while
    /// still loading. Avoids a `String` + `Arc<str>` allocation on every
    /// render frame (which can fire up to 120 times/s during animations).
    cached_brand: Option<SharedString>,
}

impl Shell {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        // Home owns the agent-spawn TextInput, so it needs window.
        let home = cx.new(|cx| DashboardHome::new(state.clone(), window, cx));
        // AgentsRoute owns the filter TextInput, so it needs window.
        let agents = cx.new(|cx| AgentsRoute::new(state.clone(), window, cx));
        // LogsRoute owns the filter TextInput, so it needs window.
        let logs = cx.new(|cx| LogsRoute::new(state.clone(), window, cx));
        // SystemRoute owns the services-section filter TextInput,
        // so it needs window.
        let system = cx.new(|cx| SystemRoute::new(state.clone(), window, cx));
        // Knowledge owns the memory-ingest TextInput, so it needs window.
        let knowledge = cx.new(|cx| KnowledgeRoute::new(state.clone(), window, cx));
        // CommandsRoute owns a TextInput (focusable widget) so it
        // needs `&mut Window` to register the focus handle.
        let commands = cx.new(|cx| CommandsRoute::new(window, cx));
        Self {
            state,
            home,
            agents,
            logs,
            system,
            knowledge,
            commands,
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

        // Lazily compute the brand string once when bootstrap data arrives.
        // Cloning a `SharedString` only bumps an `Arc` ref-count — zero
        // heap allocation on every frame after the first.
        if self.cached_brand.is_none() {
            if let Some(b) = &state_for_brand.bootstrap {
                self.cached_brand = Some(SharedString::from(format!(
                    "v{}  {}",
                    b.version, b.repo
                )));
            }
        }
        // Zero-allocation fallback: `new_static` stores a `&'static str`
        // pointer with no heap involvement at all.
        let brand_sub = self
            .cached_brand
            .clone()
            .unwrap_or_else(|| SharedString::new_static("loading…"));

        let active = state_for_brand.route;

        // Sync the macOS dock badge to the running-agents count. Only
        // calls into AppKit when the count actually changed — render
        // fires on every AppState notify, but `setBadgeLabel:` is a
        // window-server roundtrip we don't want to do on every tick.
        // No-op on non-macOS targets (see `native::set_dock_badge`).
        let running = state_for_brand.agents_running();
        if running != self.last_badge_count {
            let label = (running > 0).then(|| running.to_string());
            native::set_dock_badge(label.as_deref());
            self.last_badge_count = running;
        }

        // Build the nav strip. Each entry captures the AppState entity
        // by clone and updates `route` on click — the shell observes
        // the entity and re-renders, dispatching a new body view.
        let nav = h_flex()
            .gap_4()
            .children(Route::ALL.into_iter().map(|route| {
                let is_active = route == active;
                let label = route.label();
                let state = self.state.clone();
                div()
                    .id(label)
                    .px_2()
                    .py_1()
                    .text_color(if is_active { foreground } else { muted })
                    .border_b_2()
                    .border_color(if is_active {
                        primary
                    } else {
                        gpui::transparent_black()
                    })
                    .child(SharedString::new_static(label))
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        state.update(cx, |s, cx| {
                            s.set_route(route);
                            cx.notify();
                        });
                    })
                    .into_any_element()
            }));

        let header = h_flex()
            .gap_6()
            .px_5()
            .py_3()
            .border_b_1()
            .border_color(border)
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .text_lg()
                            .text_color(foreground)
                            .child(SharedString::new_static("crabcc · live")),
                    )
                    .child(div().text_color(muted).child(brand_sub)),
            )
            .child(nav);

        let body: AnyElement = match active {
            Route::Home => self.home.clone().into_any_element(),
            Route::Agents => self.agents.clone().into_any_element(),
            Route::Logs => self.logs.clone().into_any_element(),
            Route::System => self.system.clone().into_any_element(),
            Route::Knowledge => self.knowledge.clone().into_any_element(),
            Route::Commands => self.commands.clone().into_any_element(),
        };

        v_flex()
            .size_full()
            .bg(bg)
            .child(header)
            .child(div().flex_1().min_h(px(0.0)).child(body))
    }
}
