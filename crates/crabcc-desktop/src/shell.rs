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
}

impl Shell {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        // Home owns the agent-spawn TextInput, so it needs window.
        let home = cx.new(|cx| DashboardHome::new(state.clone(), window, cx));
        let agents = cx.new(|cx| AgentsRoute::new(state.clone(), cx));
        let logs = cx.new(|cx| LogsRoute::new(state.clone(), cx));
        let system = cx.new(|cx| SystemRoute::new(state.clone(), cx));
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
        let brand_sub = state_for_brand
            .bootstrap
            .as_ref()
            .map(|b| format!("v{}  {}", b.version, b.repo))
            .unwrap_or_else(|| "loading…".into());
        let active = state_for_brand.route;

        // Build the nav strip. Each entry captures the AppState entity
        // by clone and updates `route` on click — the shell observes
        // the entity and re-renders, dispatching a new body view.
        let nav = h_flex().gap_4().children(Route::ALL.into_iter().map(|route| {
            let is_active = route == active;
            let label = route.label();
            let state = self.state.clone();
            div()
                .id(label)
                .px_2()
                .py_1()
                .text_color(if is_active { foreground } else { muted })
                .border_b_2()
                .border_color(if is_active { primary } else { gpui::transparent_black() })
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
                    .child(
                        div()
                            .text_color(muted)
                            .child(SharedString::from(brand_sub)),
                    ),
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
