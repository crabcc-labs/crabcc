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
    k_graph::KnowledgeGraphRoute, knowledge::KnowledgeRoute, logs::LogsRoute, system::SystemRoute,
    timeline::TimelineRoute,
};
use crate::state::{AppState, Route};
use crate::toasts::ToastStrip;

pub struct Shell {
    state: Entity<AppState>,
    home: Entity<DashboardHome>,
    agents: Entity<AgentsRoute>,
    logs: Entity<LogsRoute>,
    system: Entity<SystemRoute>,
    knowledge: Entity<KnowledgeRoute>,
    commands: Entity<CommandsRoute>,
    timeline: Entity<TimelineRoute>,
    k_graph: Entity<KnowledgeGraphRoute>,
    /// In-window toast strip (track C.0). Mounted between the
    /// header and the body slot — renders nothing when
    /// `AppState::toasts` is empty so the layout stays unchanged
    /// for the common case.
    toasts: Entity<ToastStrip>,
    /// Most-recent value passed to `native::set_dock_badge`, so the
    /// render path can skip the AppKit call when the count hasn't
    /// changed. `u32::MAX` is the sentinel "never set yet" — picked
    /// instead of `Option<u32>` so the comparison is a single integer
    /// equality check on every render.
    last_badge_count: u32,
    /// Same change-detection sentinel for the menu-bar status item.
    /// Same data source as `last_badge_count` (running-agents count),
    /// but tracked separately so the two AppKit surfaces can be
    /// retired / reshuffled independently — e.g. a future revision
    /// could move the dock badge to a "new activity" indicator while
    /// keeping the status item on the agents count.
    last_status_count: u32,
    /// Cached `"v<version>  <repo>"` line, populated on the first
    /// render where `AppState::bootstrap` is `Some`. Bootstrap is a
    /// one-shot immutable value, so once we've formatted it we never
    /// need to do so again — subsequent renders just bump an Arc
    /// refcount via `clone()`. Saves a `format!()` String alloc + a
    /// `SharedString::from(String)` Arc alloc per render.
    cached_brand: Option<SharedString>,
    /// Newest toast id we've already delivered to the macOS
    /// Notification Center via `native::deliver_notification`
    /// (track C.2 first wedge). `None` until the first toast is
    /// delivered. The render path fires a system banner only when
    /// the front-of-deque toast's id is strictly greater — guards
    /// against re-firing on every observation tick. When mute is
    /// engaged the visible deque drops new pushes entirely (slice
    /// 4), so this sentinel naturally stops advancing.
    last_delivered_toast_id: Option<u64>,
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
        // needs `&mut Window` to register the focus handle. It also
        // needs the shared `AppState` entity now to dispatch runnable
        // rows via `submit_command_run`.
        let commands = cx.new(|cx| CommandsRoute::new(state.clone(), window, cx));
        // TimelineRoute owns the filter TextInput (focus handle), so
        // it needs window. Same construction shape as the other
        // input-bearing routes.
        let timeline = cx.new(|cx| TimelineRoute::new(state.clone(), window, cx));
        // KnowledgeGraphRoute now owns a filter TextInput (post-#341
        // canvas + filter strip), so it needs `window` to register
        // the focus handle — same construction as the other
        // input-bearing routes.
        let k_graph = cx.new(|cx| KnowledgeGraphRoute::new(state.clone(), window, cx));
        // No `window` argument — the strip has no focusable widgets
        // (yet). When the "Settings" entrypoint lands in slice 2+
        // it'll need `window` for that widget.
        let toasts = cx.new(|cx| ToastStrip::new(state.clone(), cx));
        Self {
            state,
            home,
            agents,
            logs,
            system,
            knowledge,
            commands,
            timeline,
            k_graph,
            toasts,
            last_badge_count: u32::MAX,
            last_status_count: u32::MAX,
            cached_brand: None,
            last_delivered_toast_id: None,
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
        // Lazily format on the first frame where bootstrap is `Some`.
        // From then on every render just clones the `SharedString`
        // (Arc refcount bump, no heap alloc). Bootstrap is the
        // one-shot `/api/bootstrap` payload — never mutated after
        // first arrival.
        if self.cached_brand.is_none() {
            if let Some(b) = state_for_brand.bootstrap.as_ref() {
                self.cached_brand = Some(SharedString::from(format!("v{}  {}", b.version, b.repo)));
            }
        }
        let brand_sub = self
            .cached_brand
            .clone()
            .unwrap_or_else(|| SharedString::new_static("loading…"));
        let active = state_for_brand.route;

        // Sync the macOS dock badge + menu-bar status item to the
        // running-agents count. Both AppKit calls hit the window
        // server, so we change-detect on `last_*_count` and only
        // round-trip when the value actually moves. No-op on
        // non-macOS targets (see `native::set_dock_badge` /
        // `native::set_status_item`).
        let running = state_for_brand.agents_running();
        if running != self.last_badge_count {
            let label = (running > 0).then(|| running.to_string());
            native::set_dock_badge(label.as_deref());
            self.last_badge_count = running;
        }
        if running != self.last_status_count {
            // Menu-bar title prepends a green-ish glyph so the user
            // knows agents are running at a glance even when the
            // window is hidden — matches the dot the agents tile
            // uses (`AgentStatus::Running` → `●`).
            let label = (running > 0).then(|| format!("\u{25CF} {running}"));
            native::set_status_item(label.as_deref());
            self.last_status_count = running;
        }

        // System rich-notification banners (track C.2 first wedge).
        // Walks the active toast deque newest-first and fires one
        // banner per toast strictly newer than the high-water mark
        // we last delivered. This makes alerts survive the
        // dashboard window being hidden — Notification Center is
        // the canonical surface for "something happened while you
        // weren't looking". Mute is enforced upstream: when
        // `AppState::toasts_muted` is set, `push_toast` skips the
        // enqueue, so the deque has nothing to deliver here.
        if let Some(newest) = state_for_brand.toasts.front() {
            let prev_mark = self.last_delivered_toast_id;
            if prev_mark != Some(newest.id) {
                if state_for_brand.echo_to_system {
                    for toast in state_for_brand.toasts.iter() {
                        if matches!(prev_mark, Some(m) if toast.id <= m) {
                            break;
                        }
                        let title = format!("crabcc · {}", toast.level.glyph());
                        native::deliver_notification(&title, &toast.message);
                    }
                }
                // Advance the sentinel regardless of whether we
                // delivered. When the user toggles echo OFF then ON
                // again, queued-but-suppressed toasts shouldn't
                // blast into Notification Center retroactively —
                // that's surprising.
                self.last_delivered_toast_id = Some(newest.id);
            }
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

        // Mute toggle for the in-window toast strip (track C.0 slice
        // 4). `●` (primary) when alerts are live, `○` (muted) when
        // muted — same character count so the header doesn't jitter
        // on toggle. Clicking flips `AppState::toasts_muted` and
        // clears any visible toasts on transition into mute (see
        // `AppState::toggle_toast_mute`).
        //
        // Both labels are pre-baked as `&'static str` literals so the
        // render path picks one with a branch — no `format!()` +
        // `SharedString::from(String)` per frame.
        let muted_state = state_for_brand.toasts_muted;
        let (alerts_label, alerts_color) = if muted_state {
            (SharedString::new_static("\u{25CB} alerts"), muted)
        } else {
            (SharedString::new_static("\u{25CF} alerts"), primary)
        };
        let state_for_alerts = self.state.clone();
        let alerts_btn = div()
            .id("toasts-mute-toggle")
            .px_2()
            .py_1()
            .text_color(alerts_color)
            .child(alerts_label)
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                state_for_alerts.update(cx, |s, cx| {
                    s.toggle_toast_mute();
                    cx.notify();
                });
            });

        // System-echo toggle for the macOS rich-notification side
        // (track C.2). When on (`↗` glyph in primary), every visible
        // toast also fires a banner via `native::deliver_notification`.
        // When off (`↗` glyph in muted), the in-window strip stays
        // alive but no banners ship — useful on a screen-sharing call
        // where notification overlays would clutter the recording.
        // Mute supersedes — if `toasts_muted` is set, no toast lands
        // in the visible deque, so this toggle is moot.
        let echo_state = state_for_brand.echo_to_system;
        let echo_color = if echo_state { primary } else { muted };
        let state_for_echo = self.state.clone();
        let echo_btn = div()
            .id("toasts-echo-toggle")
            .px_2()
            .py_1()
            .text_color(echo_color)
            .child(SharedString::new_static("\u{2197} system"))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                state_for_echo.update(cx, |s, cx| {
                    s.toggle_echo_to_system();
                    cx.notify();
                });
            });

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
            .child(nav)
            .child(alerts_btn)
            .child(echo_btn);

        let body: AnyElement = match active {
            Route::Home => self.home.clone().into_any_element(),
            Route::Agents => self.agents.clone().into_any_element(),
            Route::Logs => self.logs.clone().into_any_element(),
            Route::System => self.system.clone().into_any_element(),
            Route::Knowledge => self.knowledge.clone().into_any_element(),
            Route::Commands => self.commands.clone().into_any_element(),
            Route::Timeline => self.timeline.clone().into_any_element(),
            Route::KnowledgeGraph => self.k_graph.clone().into_any_element(),
        };

        v_flex()
            .size_full()
            .bg(bg)
            .child(header)
            // Toast strip — pinned below the header, above the body.
            // Renders nothing when `state.toasts` is empty so it
            // claims zero layout in the common case.
            .child(self.toasts.clone())
            // Wrap the body in an overflow_y_scroll container so dense
            // routes (System sections, Knowledge drawers, Logs tail) +
            // the tall Home dashboard scroll under a small window
            // instead of being clipped. `flex_1` + `min_h(0)` keeps the
            // header pinned and lets the body claim the rest. `id` is
            // required by gpui for a scrollable container so the
            // scroll-offset state survives renders.
            .child(
                div()
                    .id("shell-body-scroll")
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .child(body),
            )
    }
}
