//! DashboardHome — body content for the Home route.
//!
//! Layout (header + nav owned by `crate::shell`):
//!
//!   KPI strip   [Index] [Activity] [Agents] [Services]
//!   Tile row    [Recent activity] [Agents] [Services]
//!   Spawn row   Launch agent — prompt input + button + status
//!   Graph row   Relations graph (canvas, ≥360px tall)
//!
//! Reads from the shared `AppState` entity. `Render` runs on every
//! `cx.notify()` triggered by the SSE pump in `state.rs`.

use gpui::{
    div, prelude::*, px, Context, Entity, Hsla, IntoElement, MouseButton, Render, SharedString,
    Window,
};
use gpui_component::{h_flex, tooltip::Tooltip, v_flex, ActiveTheme};

use crate::api::types::{AgentStatus, SseActivityEvent};
use crate::routes::agent_spawn_sheet::AgentSpawnSheet;
use crate::routes::graph::GraphView;
use crate::state::{AppState, Route};
use crate::theme_helpers::op_color;

pub struct DashboardHome {
    state: Entity<AppState>,
    graph_view: Entity<GraphView>,
    /// Modal-ish launch sheet (#294 / A.9). Opened by the dashboard's
    /// "Launch agent…" CTA. The sheet self-closes on Detach / Kill /
    /// Open in Agents, so the host doesn't track its open state
    /// independently — render reads `spawn_sheet.is_open()` to decide
    /// whether to overlay it.
    spawn_sheet: Entity<AgentSpawnSheet>,
    /// Active op-pin on the Activity tile — set by clicking an op
    /// badge, cleared by clicking the active badge again or the
    /// header pin pill's `×`. Filters the activity buffer to that op
    /// before grouping. UI affordance per route, not on AppState
    /// (same call as the substring filters).
    activity_op_pin: Option<SharedString>,
    /// Active agent-pin on the Activity tile — set by clicking an
    /// `agt` badge, cleared by clicking the active badge again or
    /// the header pin pill's `×`. ANDed with `activity_op_pin` when
    /// both are set, so the user can narrow to "this op AND this
    /// agent" — common during a multi-step debug.
    activity_agent_pin: Option<SharedString>,
    /// Reusable scratch buffer for `group_activity`. Cleared and
    /// refilled on every render — keeps the spine allocation across
    /// SSE-driven `notify()`s instead of allocating fresh each frame.
    activity_buffer: Vec<ActivityGroup>,
}

impl DashboardHome {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        let graph_view = cx.new(|cx| GraphView::new(state.clone(), cx));
        let spawn_sheet = cx.new(|cx| AgentSpawnSheet::new(state.clone(), window, cx));
        // Re-render the dashboard whenever the sheet's open/phase state
        // changes — the dashboard's own render decides whether to layer
        // the sheet element on top.
        cx.observe(&spawn_sheet, |_, _, cx| cx.notify()).detach();
        Self {
            state,
            graph_view,
            spawn_sheet,
            activity_op_pin: None,
            activity_agent_pin: None,
            activity_buffer: Vec::with_capacity(8),
        }
    }

    /// Toggle activity agent-pin. Same shape as `pin_activity_op` —
    /// click the active id again to clear.
    fn pin_activity_agent(&mut self, id: SharedString) {
        if self.activity_agent_pin.as_deref() == Some(id.as_ref()) {
            self.activity_agent_pin = None;
        } else {
            self.activity_agent_pin = Some(id);
        }
    }

    /// Toggle activity op-pin. Clicking the active op clears it
    /// (saves the user hunting for the header `×` for casual
    /// narrow-then-clear).
    fn pin_activity_op(&mut self, op: SharedString) {
        if self.activity_op_pin.as_deref() == Some(op.as_ref()) {
            self.activity_op_pin = None;
        } else {
            self.activity_op_pin = Some(op);
        }
    }

    fn open_spawn_sheet(&self, cx: &mut Context<Self>) {
        self.spawn_sheet.update(cx, |sheet, cx| {
            sheet.open();
            cx.notify();
        });
    }
}

impl Render for DashboardHome {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Cross-route nav handoff: a prior render of System → AGENT
        // PROFILES staged a profile id to pre-populate. Consume up-
        // front so the spawn sheet opens with that profile selected.
        // One-shot — closing/submitting the sheet doesn't re-trigger.
        let pending_profile = self.state.update(cx, |s, _| s.take_pending_spawn_profile());
        if let Some(id) = pending_profile {
            self.spawn_sheet.update(cx, |sheet, cx| {
                sheet.open_with_profile(id);
                cx.notify();
            });
        }

        let state = self.state.read(cx);

        // gpui-component uses `secondary` for elevated panels — there's
        // no shadcn-style `card` token in this theme. Re-evaluate when
        // we adopt a `Card` component (track A.5+).
        let card = cx.theme().secondary;
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        // Cyberpunk accents from the active `Palette` global. Used
        // by per-route indicators below — under any palette these
        // pop more than the default text colour, mirroring the
        // web's `.dash-svc-cell.ok` / `.service-state.ok|down`
        // colour-coding.
        let palette = cx.global::<crate::theme::Palette>();
        let cyber_cyan = palette.cyber_cyan_hsla();
        let cyber_amber = palette.cyber_amber_hsla();

        // ── KPI strip ─────────────────────────────────────────────
        let index_kpi = match state.bootstrap.as_ref().and_then(|b| b.index.as_ref()) {
            Some(idx) => format!(
                "{} files · {} symbols",
                idx.files.unwrap_or_default(),
                idx.symbols.unwrap_or_default()
            ),
            None => "—".into(),
        };

        let activity_kpi = format!("{} hits", state.activity_total);
        let agents_kpi = format!("{}/{} running", state.agents_running(), state.agents.len());
        let services_kpi = state
            .services_reachable()
            .map(|(up, total)| format!("{up}/{total} reachable"))
            .unwrap_or_else(|| "—".into());

        // Bind `primary` here (rather than after the tile-row prep
        // block) so the navigable KPI cards below can pick it up for
        // their label colour. The later block redundantly binds
        // `theme.primary` from a re-borrow; that's fine.
        let kpi_primary = cx.theme().primary;
        let kpi_strip = h_flex()
            .gap_3()
            .px_5()
            .py_4()
            // INDEX stays static — there's no dedicated Index route;
            // the system-wide stats are bundled into the System view
            // alongside services / OTLP / kills.
            .child(kpi_card("INDEX", index_kpi, card, border, muted))
            .child(kpi_card_with_nav(
                "ACTIVITY",
                "kpi-activity-nav",
                Route::Timeline,
                activity_kpi,
                self.state.clone(),
                card,
                border,
                kpi_primary,
            ))
            .child(kpi_card_with_nav(
                "AGENTS",
                "kpi-agents-nav",
                Route::Agents,
                agents_kpi,
                self.state.clone(),
                card,
                border,
                kpi_primary,
            ))
            .child(kpi_card_with_nav(
                "SERVICES",
                "kpi-services-nav",
                Route::System,
                services_kpi,
                self.state.clone(),
                card,
                border,
                kpi_primary,
            ));

        // ── Tile row ──────────────────────────────────────────────
        // Groups consecutive same-op rows into a single visual line so
        // a burst of the same query (common during a startup outline
        // sweep) doesn't drown out the variety. Op badge is colour-coded
        // per family — see `op_color`. When `activity_op_pin` is set,
        // the buffer is pre-filtered to that op before grouping.
        let theme = cx.theme();
        let primary = theme.primary;
        let active_pin = self.activity_op_pin.clone();
        let active_agent_pin = self.activity_agent_pin.clone();
        let activity_iter = state.recent_activity.iter().filter(|e| {
            let op_match = match active_pin.as_deref() {
                None => true,
                Some(pinned) => e.op == pinned,
            };
            let agent_match = match active_agent_pin.as_deref() {
                None => true,
                Some(pinned) => e.agent_id.as_deref() == Some(pinned),
            };
            op_match && agent_match
        });
        group_activity(activity_iter, 8, &mut self.activity_buffer);
        let groups_empty = self.activity_buffer.is_empty();
        // Use `last_event_ts` as a wall-clock proxy. Same trick as the
        // Agents-route relative-age formatter — keeps chrono out of
        // the dep tree for a tiny display tweak.
        let now_ts = state.last_event_ts.unwrap_or_default();
        let entity_for_op = cx.entity();
        let activity_body: gpui::AnyElement =
            if groups_empty && (active_pin.is_some() || active_agent_pin.is_some()) {
                // Empty under an active pin — explain which one(s) so the
                // user doesn't think the buffer drained.
                let mut parts: Vec<String> = Vec::new();
                if let Some(op) = active_pin.as_deref() {
                    parts.push(format!("op={op}"));
                }
                if let Some(id) = active_agent_pin.as_deref() {
                    let trimmed: String = id.chars().take(8).collect();
                    parts.push(format!("agt={trimmed}"));
                }
                div()
                    .text_color(muted)
                    .child(SharedString::from(format!(
                        "no activity matches {}",
                        parts.join(" + ")
                    )))
                    .into_any_element()
            } else {
                v_flex()
                    .gap_1()
                    .children(self.activity_buffer.drain(..).enumerate().map(|(idx, g)| {
                        let op_color = op_color(&g.op, theme);
                        // Recency fade — newer rows render full alpha,
                        // older rows fade toward the floor. Applied to
                        // both the op-badge and the query text so the
                        // whole row dims as one unit. Muted-side meta
                        // already lives at low contrast, so leave it.
                        let age = (now_ts - g.latest_ts).max(0);
                        let alpha = fade_alpha_for_age(age);
                        let faded_op = with_alpha(op_color, alpha);
                        let faded_fg = with_alpha(theme.foreground, alpha);
                        // Click-to-pin on the op badge. Active op
                        // renders with a primary-colour border so
                        // it's recognisable even when the badge
                        // colour itself is muted (e.g. `outline`).
                        // gpui requires stateful elements to declare
                        // an id; suffixing with the row index keeps
                        // it unique per render pass without an
                        // extra alloc per group. `NamedInteger` pairs
                        // the static-backed name with the index
                        // directly — zero alloc per row per render.
                        let badge_id = gpui::ElementId::NamedInteger(
                            SharedString::new_static("activity-op"),
                            idx as u64,
                        );
                        let badge_pinned = active_pin.as_deref() == Some(g.op.as_str());
                        let badge_border = if badge_pinned {
                            primary
                        } else {
                            gpui::transparent_black()
                        };
                        let entity = entity_for_op.clone();
                        let click_op = g.op.clone();
                        // Agent badge — only renders when the activity buffer
                        // tagged this run with an agent_id (#311). Truncated
                        // to 8 chars to keep row width predictable; the full
                        // id is one route-switch away on Timeline. Click to
                        // pin / unpin filtering to this agent (parallel to
                        // op-pin on the op badge).
                        let active_agent_pin_for_badge = active_agent_pin.clone();
                        let agent_badge: gpui::AnyElement = match g.agent_id.as_ref() {
                            Some(id) => {
                                let trimmed: String = id.chars().take(8).collect();
                                let agent_pinned =
                                    active_agent_pin_for_badge.as_deref() == Some(id.as_ref());
                                let badge_color = if agent_pinned { primary } else { muted };
                                let click_id = id.clone();
                                let entity_for_agent = entity_for_op.clone();
                                let badge_id = gpui::ElementId::NamedInteger(
                                    SharedString::new_static("activity-agent"),
                                    idx as u64,
                                );
                                {
                                    let agent_tooltip: SharedString = if agent_pinned {
                                        SharedString::new_static("Unpin agent — show all activity")
                                    } else {
                                        SharedString::from(format!(
                                            "Pin agent {trimmed} — narrows the activity tile"
                                        ))
                                    };
                                    div()
                                        .id(badge_id)
                                        .px_1()
                                        .border_1()
                                        .border_color(badge_color)
                                        .rounded_md()
                                        .text_color(badge_color)
                                        .cursor_pointer()
                                        .hover(move |s| s.border_color(primary).text_color(primary))
                                        .tooltip(move |window, cx| {
                                            Tooltip::new(agent_tooltip.clone()).build(window, cx)
                                        })
                                        .child(SharedString::from(format!("agt {trimmed}")))
                                        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                            let id = click_id.clone();
                                            entity_for_agent.update(cx, |this, cx| {
                                                this.pin_activity_agent(id);
                                                cx.notify();
                                            });
                                        })
                                        .into_any_element()
                                }
                            }
                            None => div().into_any_element(),
                        };
                        h_flex()
                                .gap_2()
                                // Op badge — fixed-width column so the
                                // query text aligns across rows.
                                .child({
                                    let op_tooltip: SharedString = if badge_pinned {
                                        SharedString::new_static("Unpin op — show all activity")
                                    } else {
                                        SharedString::from(format!(
                                            "Pin op {} — narrows the activity tile",
                                            g.op
                                        ))
                                    };
                                    div()
                                        .id(badge_id)
                                        .w(px(80.0))
                                        .px_1()
                                        .border_1()
                                        .border_color(badge_border)
                                        .rounded_md()
                                        .text_color(faded_op)
                                        .cursor_pointer()
                                        .hover(move |s| s.border_color(primary))
                                        .tooltip(move |window, cx| {
                                            Tooltip::new(op_tooltip.clone()).build(window, cx)
                                        })
                                        .child(g.op.clone())
                                        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                            let op = click_op.clone();
                                            entity.update(cx, |this, cx| {
                                                this.pin_activity_op(op);
                                                cx.notify();
                                            });
                                        })
                                })
                                .child(
                                    div()
                                        .flex_1()
                                        .text_color(faded_fg)
                                        .child(SharedString::from(truncate(&g.latest_query, 60))),
                                )
                                .child(agent_badge)
                                .child(
                                    div()
                                        .text_color(muted)
                                        .child(SharedString::from(if g.count == 1 {
                                            format!("({})", g.latest_results)
                                        } else {
                                            format!("(×{} · {})", g.count, g.latest_results)
                                        })),
                                )
                                .into_any_element()
                    }))
                    .into_any_element()
            };
        // Header pin-pills — render whenever an op or agent is pinned.
        // Each pill is the canonical clear-affordance (clicking the
        // pinned badge in a row also toggles, but a row may scroll
        // away — the pill is the stable home for "I'm filtered, get
        // me out").
        let mut pin_row = h_flex().gap_2();
        let mut have_pill = false;
        if let Some(op) = active_pin.as_ref() {
            let entity_for_clear = cx.entity();
            pin_row = pin_row.child(
                div()
                    .id("activity-op-pin-clear")
                    .px_2()
                    .py_0p5()
                    .border_1()
                    .border_color(primary)
                    .rounded_md()
                    .text_color(primary)
                    .cursor_pointer()
                    .hover(move |s| s.bg(card))
                    .tooltip(|window, cx| Tooltip::new("Clear op pin").build(window, cx))
                    .child(SharedString::from(format!("{op} \u{00D7}")))
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        entity_for_clear.update(cx, |this, cx| {
                            this.activity_op_pin = None;
                            cx.notify();
                        });
                    }),
            );
            // Op-pin → Timeline cross-link, mirroring the agent-pin
            // version below. The dashboard tile shows 8 grouped rows;
            // Timeline shows the per-call detail. From a pinned-op
            // dashboard view, this lands the user on Timeline already
            // op-filtered.
            let op_for_nav = op.clone();
            let state_for_nav = self.state.clone();
            pin_row = pin_row.child(
                div()
                    .id("activity-op-pin-to-timeline")
                    .px_2()
                    .py_0p5()
                    .border_1()
                    .border_color(border)
                    .rounded_md()
                    .text_color(muted)
                    .cursor_pointer()
                    .hover(move |s| s.border_color(primary).text_color(primary))
                    .tooltip(|window, cx| {
                        Tooltip::new("Open Timeline filtered to this op").build(window, cx)
                    })
                    .child(SharedString::new_static("\u{2192} Timeline"))
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        let op = op_for_nav.clone();
                        state_for_nav.update(cx, |s, cx| {
                            s.navigate_to_timeline_with_op_pin(op);
                            cx.notify();
                        });
                    }),
            );
            have_pill = true;
        }
        if let Some(id) = active_agent_pin.as_ref() {
            let entity_for_clear = cx.entity();
            let trimmed: String = id.chars().take(8).collect();
            pin_row = pin_row.child(
                div()
                    .id("activity-agent-pin-clear")
                    .px_2()
                    .py_0p5()
                    .border_1()
                    .border_color(primary)
                    .rounded_md()
                    .text_color(primary)
                    .cursor_pointer()
                    .hover(move |s| s.bg(card))
                    .tooltip(|window, cx| Tooltip::new("Clear agent pin").build(window, cx))
                    .child(SharedString::from(format!("agt {trimmed} \u{00D7}")))
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        entity_for_clear.update(cx, |this, cx| {
                            this.activity_agent_pin = None;
                            cx.notify();
                        });
                    }),
            );
            // Cross-route dive — pre-applies the same agent_pin on
            // the Timeline route via the AppState handoff slot, so
            // the user lands on Timeline already filtered to this
            // agent. The dashboard tile shows 8 grouped rows; the
            // Timeline shows the per-call detail.
            let id_for_nav = id.clone();
            let state_for_nav = self.state.clone();
            pin_row = pin_row.child(
                div()
                    .id("activity-agent-pin-to-timeline")
                    .px_2()
                    .py_0p5()
                    .border_1()
                    .border_color(border)
                    .rounded_md()
                    .text_color(muted)
                    .cursor_pointer()
                    .hover(move |s| s.border_color(primary).text_color(primary))
                    .tooltip(|window, cx| {
                        Tooltip::new("Open Timeline filtered to this agent").build(window, cx)
                    })
                    .child(SharedString::new_static("\u{2192} Timeline"))
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        let id = id_for_nav.clone();
                        state_for_nav.update(cx, |s, cx| {
                            s.navigate_to_timeline_with_agent_pin(id);
                            cx.notify();
                        });
                    }),
            );
            have_pill = true;
        }
        let pin_pill: gpui::AnyElement = if have_pill {
            pin_row.into_any_element()
        } else {
            div().into_any_element()
        };
        let activity_tile = tile_with_nav(
            "Recent activity",
            "activity-tile-heading-nav",
            Route::Timeline,
            self.state.clone(),
            card,
            border,
            muted,
            primary,
            v_flex().gap_2().child(pin_pill).child(activity_body),
        );

        // Agents tile gets a per-row Kill button for *running* agents.
        // Exited rows just show the dot + id + runtime; clicking nothing
        // useful would be misleading. Each Kill button captures the
        // agent id by clone and dispatches `submit_kill` through the
        // shared `AppState` entity.
        let danger = cx.theme().danger;
        let foreground_for_kill = cx.theme().foreground;
        let agents_state = self.state.clone();
        let agents_tile = tile_with_nav(
            "Agents",
            "agents-tile-heading-nav",
            Route::Agents,
            self.state.clone(),
            card,
            border,
            muted,
            primary,
            v_flex()
                .gap_1()
                .children(state.agents.iter().take(8).map(|a| {
                    let dot = match a.status {
                        AgentStatus::Running => "●",
                        AgentStatus::Exited => "○",
                    };
                    let dot_color = match a.status {
                        AgentStatus::Running => cyber_cyan,
                        AgentStatus::Exited => muted,
                    };
                    let kill_btn: gpui::AnyElement = if matches!(a.status, AgentStatus::Running) {
                        let id_for_click = a.id.clone();
                        let id_for_tooltip: SharedString =
                            a.id.chars().take(8).collect::<String>().into();
                        let state_for_click = agents_state.clone();
                        // Pre-computed at SSE-decode time — no
                        // per-render `format!()` alloc. See
                        // `AgentDerived` in `api/types.rs`.
                        let element_id: gpui::ElementId = a.derived.kill_id_home.clone().into();
                        div()
                            .id(element_id)
                            .px_2()
                            .py_0p5()
                            .border_1()
                            .border_color(danger)
                            .rounded_md()
                            .text_color(danger)
                            .cursor_pointer()
                            .hover(move |s| s.bg(danger).text_color(foreground_for_kill))
                            .tooltip(move |window, cx| {
                                Tooltip::new(SharedString::from(format!(
                                    "Kill agent {id_for_tooltip}"
                                )))
                                .build(window, cx)
                            })
                            .child(SharedString::new_static("Kill"))
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                state_for_click.read(cx).submit_kill(id_for_click.clone());
                            })
                            .into_any_element()
                    } else {
                        div().into_any_element()
                    };
                    h_flex()
                            .gap_2()
                            // Running dot picks up the palette's
                            // cyber_cyan accent — bright cyan under
                            // CYBERPUNK_NEON / WEB_DARK / WEB_LIGHT,
                            // mid-grey under MONO, saturated cyan
                            // under HIGH_CONTRAST. Exited stays muted.
                            .child(div().text_color(dot_color).child(SharedString::from(dot.to_string())))
                            // a.id is now SharedString — clone is a refcount
                            // bump, no allocation per render.
                            .child(a.id.clone())
                            .child(div().text_color(muted).child(
                                a.runtime.clone().unwrap_or_else(|| "—".into()),
                            ))
                            .child(div().flex_1())
                            .child(kill_btn)
                            .into_any_element()
                })),
        );

        // Hoist the Some/None match outside `.children()` so each arm
        // can call its own builder method (children-iter vs single
        // child) — drops the `Vec<AnyElement>` round-trip both arms
        // were paying for type unification.
        let services_body = v_flex().gap_1();
        let services_body = match state.services.as_ref() {
            Some(rep) => services_body.children(rep.services.iter().take(10).map(|s| {
                let mark = if s.reachable { "✓" } else { "✗" };
                let mark_color = if s.reachable { cyber_cyan } else { cyber_amber };
                h_flex()
                    .gap_2()
                    // Reachable / down indicator picks up the
                    // palette's cyber_cyan / cyber_amber accents,
                    // mirroring the web's `.service-state.ok|down`
                    // treatment. Gives a per-row at-a-glance
                    // service-health read.
                    .child(div().text_color(mark_color).child(SharedString::from(mark.to_string())))
                    .child(s.name.clone())
                    .child(
                        div()
                            .text_color(muted)
                            .child(SharedString::from(format!("{}ms", s.latency_ms))),
                    )
                    .into_any_element()
            })),
            None => services_body.child(
                div()
                    .text_color(muted)
                    .child(SharedString::new_static("loading…")),
            ),
        };
        let services_tile = tile("Services", card, border, muted, services_body);

        // Memory tile — preview of the last 5 drawers; mirrors the
        // web `<DashTile title="memory drawers">` pattern (link to
        // /knowledge for the full surface). Three states match the
        // web: backend-not-bootstrapped, empty, populated.
        let memory_body: gpui::AnyElement = match state.memory_recent.as_ref() {
            None => div()
                .text_color(muted)
                .child(SharedString::new_static("loading…"))
                .into_any_element(),
            Some(rep) if !rep.present => div()
                .text_color(muted)
                .child(SharedString::new_static(
                    "no drawer db — run `crabcc memory init`",
                ))
                .into_any_element(),
            Some(rep) if rep.drawers.is_empty() => div()
                .text_color(muted)
                .child(SharedString::new_static("no recent drawers"))
                .into_any_element(),
            Some(rep) => v_flex()
                .gap_1()
                .children(rep.drawers.iter().take(5).map(|d| {
                    let age_secs = (now_ts - d.created_at).max(0);
                    let age = fmt_age_short(age_secs);
                    h_flex()
                        .gap_2()
                        // Wing badge — colour-cue keyed off the wing
                        // value's first byte, matching the home
                        // activity tile's op-colour treatment.
                        .child(
                            div()
                                .px_1()
                                .border_1()
                                .border_color(muted)
                                .rounded_md()
                                .text_color(muted)
                                .child(d.wing.clone()),
                        )
                        .child(div().flex_1().child(d.source_id.clone()))
                        .child(div().text_color(muted).child(SharedString::from(age)))
                        .into_any_element()
                }))
                .into_any_element(),
        };
        let memory_meta: Option<gpui::Div> =
            state.memory_recent.as_ref().filter(|r| r.present).map(|r| {
                div()
                    .text_color(muted)
                    .text_xs()
                    .child(SharedString::from(format!("{} total", r.drawers.len())))
            });
        let memory_tile = tile_with_meta("Memory", memory_meta, card, border, muted, memory_body);

        let tile_row = h_flex()
            .gap_3()
            .px_5()
            .py_2()
            .child(activity_tile)
            .child(agents_tile)
            .child(services_tile)
            .child(memory_tile);

        // ── Spawn-agent CTA ────────────────────────────────────────
        // The launch flow lives in `AgentSpawnSheet` now (#294). The
        // dashboard just owns a button that opens the sheet, plus a
        // status_line that surfaces the most recent server response so
        // failed launches don't disappear silently if the user has
        // already detached.
        let primary = cx.theme().primary;
        let success = cx.theme().success;
        let danger = cx.theme().danger;
        let foreground = cx.theme().foreground;
        let view_entity = cx.entity();
        let launch_btn = div()
            .id("agent-launch-open-sheet")
            .px_3()
            .py_1()
            .border_1()
            .border_color(primary)
            .rounded_md()
            .text_color(primary)
            .cursor_pointer()
            .hover(move |s| s.bg(primary).text_color(foreground))
            .tooltip(|window, cx| Tooltip::new("Open agent spawn sheet").build(window, cx))
            .child(SharedString::new_static("Launch agent…"))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                view_entity.update(cx, |this, cx| this.open_spawn_sheet(cx));
            });
        let status_line: gpui::AnyElement = match state.last_launch.as_ref() {
            None => div().into_any_element(),
            Some(Ok(msg)) => div()
                .text_color(success)
                .child(SharedString::from(msg.clone()))
                .into_any_element(),
            Some(Err(msg)) => div()
                .text_color(danger)
                .child(SharedString::from(msg.clone()))
                .into_any_element(),
        };
        let spawn_row = v_flex()
            .px_5()
            .py_2()
            .gap_1()
            .child(h_flex().gap_2().child(launch_btn))
            .child(status_line);

        let graph_row = div().px_5().py_2().child(self.graph_view.clone());

        // Wrap the route body in a `relative()` container so the spawn
        // sheet can overlay it via `.absolute()` without affecting the
        // dashboard's flex layout. The sheet element renders an empty
        // div when `is_open == false`, so this overlay is zero-cost
        // when the sheet is closed.
        let sheet_open = self.spawn_sheet.read(cx).is_open();
        let body = v_flex()
            .size_full()
            .child(kpi_strip)
            .child(tile_row)
            .child(spawn_row)
            .child(graph_row);

        let mut shell = div().relative().size_full().child(body);
        if sheet_open {
            shell = shell.child(self.spawn_sheet.clone());
        }
        shell
    }
}

fn kpi_card(
    label: &'static str,
    value: String,
    card_bg: gpui::Hsla,
    border: gpui::Hsla,
    muted: gpui::Hsla,
) -> gpui::Div {
    // Mirrors the web's `.dash-tile-title` pattern — small,
    // uppercase, muted. Web also uses `letter-spacing: .06em`,
    // but gpui has no letter-spacing API in this version, so
    // uppercase + muted-fg carry the visual hierarchy alone.
    // Followed by a larger value line for the actual stat.
    v_flex()
        .min_w(px(180.0))
        .p_3()
        .gap_1()
        .bg(card_bg)
        .border_1()
        .border_color(border)
        .rounded_md()
        .child(
            div()
                .text_xs()
                .text_color(muted)
                .child(SharedString::from(label.to_uppercase())),
        )
        .child(div().text_xl().child(SharedString::from(value)))
}

/// KPI card variant whose label is a clickable nav target. Same
/// visual shape as `kpi_card` but the label uses `primary` colour
/// with a trailing `→` to signal "this drills into the full route."
/// Body click target is the whole card so the user doesn't have to
/// aim at the small label.
#[allow(clippy::too_many_arguments)]
fn kpi_card_with_nav(
    label: &'static str,
    nav_id: &'static str,
    nav_route: Route,
    value: String,
    state: Entity<AppState>,
    card_bg: gpui::Hsla,
    border: gpui::Hsla,
    primary: gpui::Hsla,
) -> gpui::Stateful<gpui::Div> {
    let nav_state = state.clone();
    let nav_tooltip: SharedString =
        SharedString::from(format!("Open {} route", label.to_lowercase()));
    div()
        .id(nav_id)
        .min_w(px(180.0))
        .p_3()
        .bg(card_bg)
        .border_1()
        .border_color(border)
        .rounded_md()
        .cursor_pointer()
        .hover(move |s| s.border_color(primary))
        .tooltip(move |window, cx| Tooltip::new(nav_tooltip.clone()).build(window, cx))
        .child(
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(primary)
                        .child(SharedString::from(format!(
                            "{} \u{2192}",
                            label.to_uppercase()
                        ))),
                )
                .child(div().text_xl().child(SharedString::from(value))),
        )
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            nav_state.update(cx, |s, cx| {
                s.set_route(nav_route);
                cx.notify();
            });
        })
}

fn tile(
    title: &'static str,
    card_bg: gpui::Hsla,
    border: gpui::Hsla,
    muted: gpui::Hsla,
    body: impl IntoElement,
) -> gpui::Div {
    tile_with_meta::<gpui::Div>(title, None, card_bg, border, muted, body)
}

/// Like `tile` but the heading is clickable and navigates to
/// `nav_route`. Used for tiles that condense data also viewable in
/// a dedicated route (Recent activity → Timeline; Agents tile →
/// Agents route). The heading text picks up `primary` colour as a
/// hover-affordance hint without a fixed underline — gpui doesn't
/// give us cheap per-element :hover yet.
#[allow(clippy::too_many_arguments)]
fn tile_with_nav(
    title: &'static str,
    nav_id: &'static str,
    nav_route: Route,
    state: Entity<AppState>,
    card_bg: gpui::Hsla,
    border: gpui::Hsla,
    muted: gpui::Hsla,
    primary: gpui::Hsla,
    body: impl IntoElement,
) -> gpui::Div {
    let nav_state = state.clone();
    let nav_tooltip: SharedString =
        SharedString::from(format!("Open {} route", title.to_lowercase()));
    // Tile header sits on `card_bg`, so hover tints to `border`
    // (the lighter neighbour) — reads as a "raised" interactive
    // surface, same convention as the toast strip footer (#407).
    let header = h_flex().items_center().justify_between().child(
        div()
            .id(nav_id)
            .px_1p5()
            .py_0p5()
            .rounded_md()
            .text_xs()
            .text_color(primary)
            .cursor_pointer()
            .hover(move |s| s.bg(border))
            .tooltip(move |window, cx| Tooltip::new(nav_tooltip.clone()).build(window, cx))
            .child(SharedString::from(format!(
                "{} \u{2192}",
                title.to_uppercase()
            )))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                nav_state.update(cx, |s, cx| {
                    s.set_route(nav_route);
                    cx.notify();
                });
            }),
    );
    let _ = muted;
    v_flex()
        .flex_1()
        .min_h(px(220.0))
        .p_3()
        .gap_2()
        .bg(card_bg)
        .border_1()
        .border_color(border)
        .rounded_md()
        .child(header)
        .child(body)
}

/// Tile variant with an optional metadata pill rendered on the
/// right of the header row — mirrors the web `<DashTile>`'s
/// `meta` prop. Used for "X total" / "X/Y up" badges.
fn tile_with_meta<E: IntoElement>(
    title: &'static str,
    meta: Option<E>,
    card_bg: gpui::Hsla,
    border: gpui::Hsla,
    muted: gpui::Hsla,
    body: impl IntoElement,
) -> gpui::Div {
    let header = h_flex().items_center().justify_between().child(
        div()
            .text_xs()
            .text_color(muted)
            .child(SharedString::from(title.to_uppercase())),
    );
    let header = match meta {
        Some(m) => header.child(m),
        None => header,
    };
    v_flex()
        .flex_1()
        .min_h(px(220.0))
        .p_3()
        .gap_2()
        .bg(card_bg)
        .border_1()
        .border_color(border)
        .rounded_md()
        .child(header)
        .child(body)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}

/// Compact age formatter — mirrors the web's `fmtAge` selector
/// (`crabcc-viz/web/src/components/dashboard/selectors.ts`).
/// `<1m → "Xs"`, `<1h → "Xm"`, `<1d → "Xh"`, else `"Xd"`.
fn fmt_age_short(secs: i64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3_600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// One visible row in the Recent Activity tile after consecutive
/// same-op events have been collapsed.
struct ActivityGroup {
    op: SharedString,
    /// `agent_id` of the run this group represents (`Some(_)` once
    /// #311 lands an agent-tagged event into the buffer). Two
    /// consecutive same-`op` events with different `agent_id` values
    /// must NOT fold together — otherwise the row's count + latest_*
    /// would silently mix cross-agent activity. `None` matches `None`
    /// (CLI / MCP rows without an agent owner).
    agent_id: Option<SharedString>,
    latest_query: SharedString,
    latest_results: u64,
    /// Timestamp of the freshest event in the run. Drives the
    /// recency-fade in the render path so newer rows render at full
    /// opacity and older ones fade toward muted.
    latest_ts: i64,
    count: usize,
}

/// Walk the buffer newest-first, collapsing runs of the same `(op,
/// agent_id)` pair into a single group whose `count` carries the run
/// length and whose `latest_*` fields show the most-recent event in
/// the run. Fills `out` with up to `cap` groups.
///
/// Splitting on `agent_id` matters once #311 tags events: without it,
/// two agents both running `sym Store` in succession fold into one
/// row, hiding which one did what.
///
/// Caller-owned `out` so the spine `Vec<ActivityGroup>` survives
/// across renders. Cleared on entry; re-filled in place. The inner
/// `SharedString` fields can still be `drain(..)`-ed by the caller
/// without losing the spine capacity.
fn group_activity<'a, I>(events: I, cap: usize, out: &mut Vec<ActivityGroup>)
where
    I: IntoIterator<Item = &'a SseActivityEvent>,
    I::IntoIter: DoubleEndedIterator,
{
    out.clear();
    for evt in events.into_iter().rev() {
        if let Some(last) = out.last_mut() {
            // Compare by ref text so `Some("a") == Some("a")` even
            // across SharedString clones; None matches None.
            let same_agent = match (last.agent_id.as_deref(), evt.agent_id.as_deref()) {
                (Some(a), Some(b)) => a == b,
                (None, None) => true,
                _ => false,
            };
            if last.op == evt.op && same_agent {
                // Same op + agent as the previous-newest group — extend
                // it. We already stored the *latest* (newest) event of
                // the run in `latest_*` since that came first in our
                // walk.
                last.count += 1;
                continue;
            }
        }
        if out.len() == cap {
            break;
        }
        out.push(ActivityGroup {
            op: evt.op.clone(),
            agent_id: evt.agent_id.clone(),
            latest_query: evt.query.clone(),
            latest_results: evt.results,
            latest_ts: evt.ts,
            count: 1,
        });
    }
}

/// Map a row's age (seconds since `now_ts`) to a multiplicative alpha
/// for the recency-fade. Rows fresher than [`FADE_FRESH_SECS`] render
/// at full opacity; rows older than [`FADE_STALE_SECS`] floor at
/// [`FADE_FLOOR_ALPHA`]; in-between fades linearly. Tuning rationale
/// in the constants' doc comments.
fn fade_alpha_for_age(age_secs: i64) -> f32 {
    if age_secs <= FADE_FRESH_SECS {
        return 1.0;
    }
    if age_secs >= FADE_STALE_SECS {
        return FADE_FLOOR_ALPHA;
    }
    let span = (FADE_STALE_SECS - FADE_FRESH_SECS) as f32;
    let into = (age_secs - FADE_FRESH_SECS) as f32;
    let t = (into / span).clamp(0.0, 1.0);
    1.0 - t * (1.0 - FADE_FLOOR_ALPHA)
}

/// Anything within this many seconds of `now` renders at full
/// opacity — short enough that activity in the last poll tick stays
/// crisp.
const FADE_FRESH_SECS: i64 = 5;
/// Above this many seconds, rows render at [`FADE_FLOOR_ALPHA`].
/// Tuned to match the activity-buffer churn rate — at typical work
/// pace the bottom of an 8-row buffer is ~30s old.
const FADE_STALE_SECS: i64 = 60;
/// Floor alpha for the oldest visible row. Kept above 0.5 so the
/// row stays legible — the fade is a "weight" cue, not "hide" cue.
const FADE_FLOOR_ALPHA: f32 = 0.55;

fn with_alpha(c: Hsla, a: f32) -> Hsla {
    Hsla { a, ..c }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evt(op: &str, q: &str, results: u64) -> SseActivityEvent {
        SseActivityEvent {
            ts: 0,
            op: op.into(),
            query: q.into(),
            results,
            agent_id: None,
        }
    }

    #[test]
    fn grouping_collapses_consecutive_runs() {
        // Buffer is oldest→newest; group_activity walks newest-first.
        let events = vec![
            evt("outline", "a", 1),
            evt("outline", "b", 2),
            evt("outline", "c", 3),
            evt("sym", "Store", 1),
            evt("refs", "Store", 2),
            evt("refs", "Index", 3),
        ];
        let mut groups = Vec::new();
        group_activity(&events, 8, &mut groups);
        // Expected (newest first): refs ×2 (latest=Index), sym ×1, outline ×3 (latest=c)
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].op, "refs");
        assert_eq!(groups[0].count, 2);
        assert_eq!(groups[0].latest_query, "Index");
        assert_eq!(groups[1].op, "sym");
        assert_eq!(groups[1].count, 1);
        assert_eq!(groups[2].op, "outline");
        assert_eq!(groups[2].count, 3);
        assert_eq!(groups[2].latest_query, "c");
    }

    #[test]
    fn grouping_caps_at_visible_count() {
        let events: Vec<SseActivityEvent> = (0..20)
            .map(|i| evt(&format!("op-{i}"), "q", i as u64))
            .collect();
        let mut groups = Vec::new();
        group_activity(&events, 5, &mut groups);
        // Each event has a unique op, so groups equal events. We expect
        // exactly 5 (the cap) — the *newest* 5.
        assert_eq!(groups.len(), 5);
        assert_eq!(groups[0].op, "op-19");
        assert_eq!(groups[4].op, "op-15");
    }

    #[test]
    fn grouping_handles_empty_input() {
        let mut groups: Vec<ActivityGroup> = Vec::new();
        group_activity(&[] as &[SseActivityEvent], 8, &mut groups);
        assert!(groups.is_empty());
    }

    fn evt_a(op: &str, q: &str, results: u64, agent: Option<&str>) -> SseActivityEvent {
        SseActivityEvent {
            ts: 0,
            op: op.into(),
            query: q.into(),
            results,
            agent_id: agent.map(Into::into),
        }
    }

    #[test]
    fn grouping_breaks_on_agent_change_even_with_same_op() {
        // Agent A and Agent B both run sym Store consecutively.
        // Without agent-aware grouping these would fold into one row,
        // hiding which agent did what. Buffer is oldest → newest.
        let events = vec![
            evt_a("sym", "Store", 1, Some("agent-a")),
            evt_a("sym", "Store", 2, Some("agent-b")),
        ];
        let mut groups = Vec::new();
        group_activity(&events, 8, &mut groups);
        // Newest-first: agent-b row, then agent-a row. Two distinct
        // groups even though `op` matches.
        assert_eq!(groups.len(), 2);
        assert_eq!(
            groups[0].agent_id.as_deref().map(|s| s.to_string()),
            Some("agent-b".into())
        );
        assert_eq!(
            groups[1].agent_id.as_deref().map(|s| s.to_string()),
            Some("agent-a".into())
        );
        assert_eq!(groups[0].count, 1);
        assert_eq!(groups[1].count, 1);
    }

    #[test]
    fn grouping_folds_same_op_within_one_agent() {
        // Same op + same agent over 3 events should fold to one
        // group of count 3.
        let events = vec![
            evt_a("sym", "a", 1, Some("agent-a")),
            evt_a("sym", "b", 2, Some("agent-a")),
            evt_a("sym", "c", 3, Some("agent-a")),
        ];
        let mut groups = Vec::new();
        group_activity(&events, 8, &mut groups);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].count, 3);
        assert_eq!(groups[0].latest_query, "c");
    }

    #[test]
    fn grouping_breaks_when_only_one_side_has_agent_id() {
        // Some(_) and None must not fold even when op matches —
        // mixing CLI invocations into an agent's run obscures both.
        let events = vec![
            evt_a("sym", "x", 1, None),
            evt_a("sym", "y", 2, Some("agent-a")),
        ];
        let mut groups = Vec::new();
        group_activity(&events, 8, &mut groups);
        assert_eq!(groups.len(), 2);
        assert!(groups[0].agent_id.is_some());
        assert!(groups[1].agent_id.is_none());
    }

    #[test]
    fn grouping_clears_existing_buffer_on_entry() {
        // Buffer-reuse contract: a fresh call clears prior contents
        // and refills in place — and the spine capacity survives so
        // the steady-state allocation count is zero.
        let mut groups = Vec::with_capacity(16);
        let first = vec![evt("sym", "x", 1)];
        group_activity(&first, 8, &mut groups);
        assert_eq!(groups.len(), 1);
        let cap_before = groups.capacity();

        let second = vec![evt("refs", "y", 2), evt("callers", "z", 3)];
        group_activity(&second, 8, &mut groups);
        // Old "sym" entry must be gone — buffer was cleared, not
        // appended.
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].op, "callers");
        assert_eq!(groups[1].op, "refs");
        // Spine capacity preserved (the whole point of the param flip).
        assert!(groups.capacity() >= cap_before);
    }
}
