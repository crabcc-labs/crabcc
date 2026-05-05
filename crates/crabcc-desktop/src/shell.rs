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
use gpui_component::{h_flex, tooltip::Tooltip, v_flex, ActiveTheme};

use crate::about::AboutModal;
use crate::native;
use crate::routes::{
    agents::AgentsRoute, commands::CommandsRoute, dashboard::DashboardHome,
    k_graph::KnowledgeGraphRoute, knowledge::KnowledgeRoute, logs::LogsRoute, system::SystemRoute,
    timeline::TimelineRoute,
};
use crate::settings::SettingsPanel;
use crate::state::{AppState, Route};
use crate::theme::Palette;
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
    /// Inline settings panel — opens via the header gear button.
    /// Mounted just below the toast strip; renders nothing when
    /// closed so the body keeps full height.
    settings: Entity<SettingsPanel>,
    /// About modal — opened from the settings panel's
    /// "About crabcc-desktop ›" link. Mounted at the very end
    /// of the v_flex chain so the absolute-positioned overlay
    /// covers everything below it.
    about: Entity<AboutModal>,
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
    /// Last `window.set_window_title` argument, so the render path
    /// can skip the platform call when the active route hasn't
    /// changed. `None` is the "never set" sentinel — the first
    /// render always pushes the route-derived title.
    last_window_title: Option<&'static str>,
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
        // KnowledgeGraphRoute is a stub today — no focusable widgets
        // because the canvas isn't rendered yet (server-side
        // `/api/memory/graph` blocks #297). Owns no TextInput, so
        // `window` is not threaded in.
        let k_graph = cx.new(|cx| KnowledgeGraphRoute::new(state.clone(), cx));
        // No `window` argument — the strip has no focusable widgets
        // (yet). When the "Settings" entrypoint lands in slice 2+
        // it'll need `window` for that widget.
        let toasts = cx.new(|cx| ToastStrip::new(state.clone(), cx));
        let about = cx.new(AboutModal::new);
        let settings = cx.new(|cx| SettingsPanel::new(state.clone(), about.clone(), cx));
        // Re-render the shell when the about modal toggles so
        // the overlay paints / unpaints in step.
        cx.observe(&about, |_, _, cx| cx.notify()).detach();
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
            settings,
            about,
            last_badge_count: u32::MAX,
            last_status_count: u32::MAX,
            cached_brand: None,
            last_delivered_toast_id: None,
            last_window_title: None,
        }
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bg = cx.theme().background;
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let foreground = cx.theme().foreground;
        let primary = cx.theme().primary;
        // Shared hover background for clickable header items + nav tabs.
        // The `secondary` token is the panel-ish surface used by the
        // settings panel + tile bodies, so it already reads as
        // "interactive surface" in every palette.
        let hover_bg = cx.theme().secondary;

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

        // Sync the OS window title to the active route, so the
        // taskbar / Dock-tooltip / Cmd-Tab switcher reflects what
        // the user is looking at without having to surface the
        // window. Static per route — `last_window_title` is a
        // `&'static str` ptr equality check so the platform call
        // only fires on actual route change.
        let desired_title = active.window_title();
        if self.last_window_title != Some(desired_title) {
            window.set_window_title(desired_title);
            self.last_window_title = Some(desired_title);
        }

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

        // Live-count badges next to the Agents / System tabs.
        // Mirrors the dock badge + Home dashboard tiles so the user
        // can read state at a glance from any route. Cyber accents
        // come from the active palette (`cyber_cyan` for healthy
        // running counts, `cyber_amber` for degraded services) so
        // the badges follow palette switches without their own
        // colour table.
        let palette = cx.global::<Palette>();
        let badge_running = palette.cyber_cyan_hsla();
        let badge_degraded = palette.cyber_amber_hsla();
        let services_status = state_for_brand.services_reachable();

        // Build the nav strip. Each entry captures the AppState entity
        // by clone and updates `route` on click — the shell observes
        // the entity and re-renders, dispatching a new body view.
        let nav = h_flex()
            .gap_4()
            .children(Route::ALL.into_iter().map(|route| {
                let is_active = route == active;
                let label = route.label();
                let state = self.state.clone();

                // Optional live-count badge — `Some((text, colour))`
                // when the route has a glanceable signal worth
                // surfacing without leaving the current view.
                let badge: Option<(SharedString, gpui::Hsla)> = match route {
                    Route::Agents if running > 0 => {
                        Some((SharedString::from(running.to_string()), badge_running))
                    }
                    Route::System => match services_status {
                        Some((up, total)) if up < total => {
                            Some((SharedString::from(format!("{up}/{total}")), badge_degraded))
                        }
                        _ => None,
                    },
                    _ => None,
                };

                let mut tab_inner = h_flex().gap_2().child(SharedString::new_static(label));
                if let Some((badge_text, badge_color)) = badge {
                    tab_inner =
                        tab_inner.child(div().text_xs().text_color(badge_color).child(badge_text));
                }

                let tab_tooltip: SharedString = if is_active {
                    SharedString::from(format!("On {label} — click to keep"))
                } else {
                    SharedString::from(format!("Go to {label}"))
                };

                div()
                    .id(label)
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_color(if is_active { foreground } else { muted })
                    .border_b_2()
                    .border_color(if is_active {
                        primary
                    } else {
                        gpui::transparent_black()
                    })
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover_bg))
                    .tooltip(move |window, cx| Tooltip::new(tab_tooltip.clone()).build(window, cx))
                    .child(tab_inner)
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
        // State-aware tooltip — describes the click outcome, not the
        // toggle. `SharedString::new_static` is fine since both
        // strings are compile-time literals; the tooltip closure
        // captures by clone so the `move |window, cx|` doesn't fight
        // the borrow checker.
        let alerts_tooltip: SharedString = if muted_state {
            SharedString::new_static("Unmute alerts")
        } else {
            SharedString::new_static("Mute alerts")
        };
        let alerts_btn = div()
            .id("toasts-mute-toggle")
            .px_2()
            .py_1()
            .rounded_md()
            .text_color(alerts_color)
            .cursor_pointer()
            .hover(move |s| s.bg(hover_bg))
            .tooltip(move |window, cx| Tooltip::new(alerts_tooltip.clone()).build(window, cx))
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
        let echo_tooltip: SharedString = if echo_state {
            SharedString::new_static("Stop echoing alerts to Notification Center")
        } else {
            SharedString::new_static("Echo alerts to macOS Notification Center")
        };
        let state_for_echo = self.state.clone();
        let echo_btn = div()
            .id("toasts-echo-toggle")
            .px_2()
            .py_1()
            .rounded_md()
            .text_color(echo_color)
            .cursor_pointer()
            .hover(move |s| s.bg(hover_bg))
            .tooltip(move |window, cx| Tooltip::new(echo_tooltip.clone()).build(window, cx))
            .child(SharedString::new_static("\u{2197} system"))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                state_for_echo.update(cx, |s, cx| {
                    s.toggle_echo_to_system();
                    cx.notify();
                });
            });

        // Palette switcher — cycles through `Palette::ALL_NAMES`
        // on click, applies the new palette to the global theme,
        // and fires `window.refresh()` so every observed entity
        // re-renders with the new colours. The label is the
        // canonical palette name (`web_dark`, `cyberpunk_neon`,
        // …) so the user can always see which palette is active.
        let palette_label =
            SharedString::from(format!("\u{25D0} {}", state_for_brand.palette_name(),));
        let state_for_palette = self.state.clone();
        let palette_btn = div()
            .id("palette-cycle")
            .px_2()
            .py_1()
            .rounded_md()
            .text_color(muted)
            .cursor_pointer()
            .hover(move |s| s.bg(hover_bg))
            .tooltip(|window, cx| Tooltip::new("Cycle theme palette").build(window, cx))
            .child(palette_label)
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                state_for_palette.update(cx, |s, cx| {
                    s.cycle_palette();
                    cx.notify();
                });
                let idx = state_for_palette.read(cx).palette_index;
                let palette = crate::theme::apply_by_index(cx, idx);
                let _ = palette; // logged inside apply if ever needed
                                 // Persist the user's choice so it survives restart.
                                 // Best-effort: errors swallowed inside (sandbox /
                                 // missing $HOME). Source of truth for next launch
                                 // is `theme::initial_palette_index`.
                let name = state_for_palette.read(cx).palette_name();
                crate::theme::save_persisted_palette(name);
                // Mutating the global theme doesn't auto-trigger
                // a redraw — entity observation only fires for
                // entity-shaped state. Force the whole window to
                // repaint so every cached ColourEntity-equivalent
                // picks up the new tokens.
                window.refresh();
            });

        // Settings gear — toggles the inline settings panel mounted
        // between the toast strip and the body. Unicode `⚙` is the
        // canonical gear glyph.
        let settings_open = self.settings.read(cx).is_open();
        let settings_color = if settings_open { primary } else { muted };
        let settings_entity = self.settings.clone();
        let settings_btn = div()
            .id("settings-toggle")
            .px_2()
            .py_1()
            .rounded_md()
            .text_color(settings_color)
            .cursor_pointer()
            .hover(move |s| s.bg(hover_bg))
            .tooltip(|window, cx| Tooltip::new("Settings").build(window, cx))
            .child(SharedString::new_static("\u{2699}"))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                settings_entity.update(cx, |panel, cx| {
                    panel.toggle();
                    cx.notify();
                });
            });

        // Brand block — clicking anywhere on "crabcc · live  v… repo"
        // jumps to Home, mirroring the convention every web dashboard
        // already trains users on. The whole block is one Stateful
        // div so the hover tint covers both the title + the version
        // line, not just the bigger label.
        let state_for_brand_click = self.state.clone();
        let brand_block = h_flex()
            .id("brand-home")
            .gap_3()
            .px_2()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .hover(move |s| s.bg(hover_bg))
            .tooltip(|window, cx| Tooltip::new("Go to Home").build(window, cx))
            .child(
                div()
                    .text_lg()
                    .text_color(foreground)
                    .child(SharedString::new_static("crabcc · live")),
            )
            .child(div().text_color(muted).child(brand_sub))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                state_for_brand_click.update(cx, |s, cx| {
                    s.set_route(Route::Home);
                    cx.notify();
                });
            });

        let header = h_flex()
            .gap_6()
            .px_5()
            .py_3()
            .border_b_1()
            .border_color(border)
            .child(brand_block)
            .child(nav)
            .child(alerts_btn)
            .child(echo_btn)
            .child(palette_btn)
            .child(settings_btn);

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

        // Status bar — thin strip pinned at the bottom of the
        // window, surfacing service health + running-agents count
        // alongside the in-window route. Same data the nav badges
        // and dock badge already carry, but at the foot of the
        // window it's the second canonical place a user looks for
        // overall app state. Renders nothing fancy — just two
        // glyph-prefixed counts in muted text with cyber accents
        // when the values are interesting.
        // Services segment — clickable shortcut to the System route.
        // Tooltip telegraphs the click target since the segment
        // itself doesn't carry a `→` hint (the foot bar should stay
        // visually quiet when nothing is happening).
        let (services_color, services_label, services_tooltip) =
            match state_for_brand.services_reachable() {
                Some((up, total)) if up < total => (
                    badge_degraded,
                    SharedString::from(format!("services {up}/{total}")),
                    SharedString::from(format!(
                        "View System — {} of {total} services unreachable",
                        total - up
                    )),
                ),
                Some((up, total)) => (
                    badge_running,
                    SharedString::from(format!("services {up}/{total}")),
                    SharedString::from(format!("View System — all {total} services up")),
                ),
                None => (
                    muted,
                    SharedString::new_static("services —"),
                    SharedString::new_static("View System — discovery still loading"),
                ),
            };
        let state_for_services_jump = self.state.clone();
        let services_segment: AnyElement = h_flex()
            .id("status-services")
            .gap_1p5()
            .px_2()
            .py_0p5()
            .rounded_md()
            .cursor_pointer()
            .hover(move |s| s.bg(hover_bg))
            .tooltip(move |window, cx| Tooltip::new(services_tooltip.clone()).build(window, cx))
            .child(
                div()
                    .text_color(services_color)
                    .child(SharedString::new_static("\u{25CF}")),
            )
            .child(div().text_color(muted).child(services_label))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                state_for_services_jump.update(cx, |s, cx| {
                    s.set_route(Route::System);
                    cx.notify();
                });
            })
            .into_any_element();

        // Agents segment — clickable shortcut to the Agents route.
        // Same chrome as services. Glyph flips to the empty `○`
        // when no agents are running so the row reads as quiet.
        let (agents_color, agents_glyph) = if running > 0 {
            (badge_running, "\u{25CF}")
        } else {
            (muted, "\u{25CB}")
        };
        let agents_label = SharedString::from(format!("agents {running}"));
        let agents_tooltip: SharedString = if running > 0 {
            SharedString::from(format!(
                "View Agents — {running} running agent{}",
                if running == 1 { "" } else { "s" }
            ))
        } else {
            SharedString::new_static("View Agents — none running")
        };
        let state_for_agents_jump = self.state.clone();
        let agents_segment: AnyElement = h_flex()
            .id("status-agents")
            .gap_1p5()
            .px_2()
            .py_0p5()
            .rounded_md()
            .cursor_pointer()
            .hover(move |s| s.bg(hover_bg))
            .tooltip(move |window, cx| Tooltip::new(agents_tooltip.clone()).build(window, cx))
            .child(
                div()
                    .text_color(agents_color)
                    .child(SharedString::new_static(agents_glyph)),
            )
            .child(div().text_color(muted).child(agents_label))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                state_for_agents_jump.update(cx, |s, cx| {
                    s.set_route(Route::Agents);
                    cx.notify();
                });
            })
            .into_any_element();
        // Right-aligned route segment: "route Logs" with the
        // active route's label. Anchors the foot of the window
        // with a "you are here" reminder, mirroring the brand
        // line's role at the top — same convention as VSCode's
        // status bar's right rail. Spacer between segments uses
        // `flex_1` so the route segment hugs the right edge
        // regardless of window width.
        let route_segment = h_flex()
            .gap_1p5()
            .child(
                div()
                    .text_color(muted)
                    .child(SharedString::new_static("route")),
            )
            .child(
                div()
                    .text_color(foreground)
                    .child(SharedString::new_static(active.label())),
            );

        // Theme segment — surfaces the active palette name AND
        // doubles as a cycle-palette shortcut, mirroring the header's
        // `◐ <palette>` button. Click-to-cycle keeps the status bar
        // honest as a "live state + quick controls" surface, same
        // pattern as the services / agents segments routing to System
        // / Agents (#420).
        let palette_name = state_for_brand.palette_name();
        let state_for_status_palette = self.state.clone();
        let theme_segment = h_flex()
            .id("status-theme-cycle")
            .gap_1p5()
            .px_2()
            .py_0p5()
            .rounded_md()
            .cursor_pointer()
            .hover(move |s| s.bg(hover_bg))
            .tooltip(|window, cx| Tooltip::new("Cycle theme palette").build(window, cx))
            .child(
                div()
                    .text_color(muted)
                    .child(SharedString::new_static("theme")),
            )
            .child(
                div()
                    .text_color(foreground)
                    .child(SharedString::new_static(palette_name)),
            )
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                state_for_status_palette.update(cx, |s, cx| {
                    s.cycle_palette();
                    cx.notify();
                });
                let idx = state_for_status_palette.read(cx).palette_index;
                let _ = crate::theme::apply_by_index(cx, idx);
                let name = state_for_status_palette.read(cx).palette_name();
                crate::theme::save_persisted_palette(name);
                window.refresh();
            });

        let status_bar = h_flex()
            .gap_4()
            .px_5()
            .py_1p5()
            .border_t_1()
            .border_color(border)
            .text_xs()
            .child(services_segment)
            .child(agents_segment)
            .child(div().flex_1())
            .child(theme_segment)
            .child(route_segment);

        v_flex()
            .size_full()
            .bg(bg)
            .child(header)
            // Toast strip — pinned below the header, above the body.
            // Renders nothing when `state.toasts` is empty so it
            // claims zero layout in the common case.
            .child(self.toasts.clone())
            // Settings panel — opens via the header gear button.
            // Renders nothing when closed so the body keeps full
            // height; when open, it sits just below the strip with
            // its own border-top + bg.
            .child(self.settings.clone())
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
            // Status bar — pinned at the foot of the window. Sits
            // between the scrollable body and the about-modal
            // overlay so it's always visible regardless of route
            // scroll state.
            .child(status_bar)
            // About modal — overlay layered on top of everything
            // when open. Renders an empty `div` when closed, so
            // it claims no layout in the common case.
            .child(self.about.clone())
    }
}
