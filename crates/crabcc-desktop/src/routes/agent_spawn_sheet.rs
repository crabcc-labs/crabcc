//! Agent-spawn sheet (#294 / A.9). Slides over the Home route when
//! the user clicks "Launch agent…". Three lifecycle phases:
//!
//!   * `Idle`      — profile picker + multi-line prompt + Launch.
//!   * `Launching` — locked input, spinner caption while the launch
//!     POST is in flight.
//!   * `Streaming` — agent id known; log tail + Kill / Detach / Open
//!     in Agents actions.
//!
//! Wires only into surfaces that already exist server-side:
//!   * `submit_launch` / `submit_kill` / `submit_agent_log` on
//!     [`AppState`].
//!   * The new `last_launch_id` field on [`AppState`], populated by
//!     the launch-result handler — the sheet observes the state and
//!     transitions out of `Launching` once the id appears.
//!
//! Streaming auto-tails the agent log every
//! [`LOG_TAIL_INTERVAL_MS`] (1.5s) via gpui's
//! `cx.background_executor().timer()` — a single async loop is
//! spawned on the `Launching → Streaming` transition and exits
//! cleanly when the phase moves anywhere else. The Refresh button
//! still works as a manual "fetch now" override.
//!
//! Both prior gaps (manual-refresh-only and profile-only-sets-model)
//! are closed: auto-tail handles the first; #306 added the wire-level
//! `profile` field for the second. No outstanding gaps tracked here.

use gpui::{
    div, prelude::*, px, Context, Entity, Focusable, IntoElement, MouseButton, Render,
    SharedString, Window,
};
use gpui_component::{
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex, ActiveTheme,
};

use crate::api::types::{AgentLaunchRequest, AgentProfileEntry};
use crate::state::{AppState, Route};

/// Sheet lifecycle. `Streaming` carries the agent id assigned by the
/// server so the action bar can target it for kill / log refresh
/// without re-walking `state.agents`.
#[derive(Debug, Clone)]
enum SheetPhase {
    Idle,
    Launching,
    Streaming { id: SharedString },
}

pub struct AgentSpawnSheet {
    state: Entity<AppState>,
    phase: SheetPhase,
    /// Whether the sheet should render at all. Toggled by the host
    /// (`DashboardHome`) via [`AgentSpawnSheet::open`] /
    /// [`AgentSpawnSheet::close`]. The sheet itself flips it to
    /// `false` on Detach / Kill / Open in Agents so the host doesn't
    /// have to remember to mirror its own copy.
    is_open: bool,
    prompt_input: Entity<InputState>,
    prompt_text: String,
    /// Selected profile id (matches `AgentProfileEntry::id`). `None`
    /// means "use server default". Click on the active row clears it.
    selected_profile: Option<SharedString>,
}

/// How often to re-poll the agent log while the sheet is in
/// `Streaming`. 1.5s is slightly slower than the SSE activity
/// cadence so we don't hammer the agent_log endpoint when nothing
/// is happening, but fast enough that a tailing user sees output
/// near-real-time. The agent's stdout is bounded — there's no
/// useful gain from sub-second polling.
const LOG_TAIL_INTERVAL_MS: u64 = 1_500;

impl AgentSpawnSheet {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Watch AppState for the launch-id arrival. The sheet stays in
        // `Launching` until the server hands back an id, then morphs
        // to `Streaming` and kicks off a first log fetch so the panel
        // isn't blank on the next render. Once in `Streaming`, also
        // spawn an auto-tail loop so subsequent log lines appear
        // without a manual Refresh click.
        cx.observe(&state, |this, st, cx| {
            if matches!(this.phase, SheetPhase::Launching) {
                let next_id = st.read(cx).last_launch_id.clone();
                if let Some(id) = next_id {
                    this.phase = SheetPhase::Streaming { id: id.clone() };
                    st.read(cx).submit_agent_log(id.clone(), 0);
                    spawn_auto_tail(cx, id);
                }
            }
            cx.notify();
        })
        .detach();

        let prompt_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .placeholder("What should the agent do?")
        });
        cx.subscribe_in(&prompt_input, window, |this, st, event, _, cx| {
            if let InputEvent::Change = event {
                this.prompt_text = st.read(cx).value().to_string();
                cx.notify();
            }
        })
        .detach();

        Self {
            state,
            phase: SheetPhase::Idle,
            is_open: false,
            prompt_input,
            prompt_text: String::new(),
            selected_profile: None,
        }
    }

    pub fn open(&mut self) {
        self.is_open = true;
    }

    pub fn close(&mut self) {
        self.is_open = false;
        self.phase = SheetPhase::Idle;
        self.selected_profile = None;
    }

    pub fn is_open(&self) -> bool {
        self.is_open
    }

    /// Toggle profile selection. Click the active row to clear — saves
    /// the user hunting for a separate "no profile" affordance.
    fn select_profile(&mut self, id: SharedString) {
        if self.selected_profile.as_deref() == Some(id.as_ref()) {
            self.selected_profile = None;
        } else {
            self.selected_profile = Some(id);
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.phase, SheetPhase::Idle) {
            return;
        }
        let prompt = self.prompt_text.trim();
        if prompt.is_empty() {
            return;
        }

        // Forward the picked profile id directly — the server (#306)
        // accepts it as a bare filename and pre-pends the `internal/`
        // namespace before passing to the spawned CLI's `--profile`
        // flag. Pre-#306 servers silently ignore the field, so this
        // is safe against an old server (the launch just runs with
        // the CLI default).
        let profile = self.selected_profile.as_ref().map(|p| p.to_string());

        let req = AgentLaunchRequest {
            prompt: prompt.to_string(),
            profile,
            ..Default::default()
        };
        self.phase = SheetPhase::Launching;
        self.state.read(cx).submit_launch(req);
        cx.notify();
    }

    fn kill(&mut self, cx: &mut Context<Self>) {
        if let SheetPhase::Streaming { id } = &self.phase {
            self.state.read(cx).submit_kill(id.clone());
        }
        self.close();
        cx.notify();
    }

    fn detach(&mut self, cx: &mut Context<Self>) {
        self.close();
        cx.notify();
    }

    fn open_in_agents(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |s, cx| {
            s.set_route(Route::Agents);
            cx.notify();
        });
        self.close();
        cx.notify();
    }

    fn refresh_log(&self, cx: &mut Context<Self>) {
        if let SheetPhase::Streaming { id } = &self.phase {
            self.state.read(cx).submit_agent_log(id.clone(), 0);
        }
    }
}

impl Render for AgentSpawnSheet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.is_open {
            return div().into_any_element();
        }

        let theme = cx.theme();
        let secondary = theme.secondary;
        let border = theme.border;
        let muted = theme.muted_foreground;
        let foreground = theme.foreground;
        let primary = theme.primary;
        let success = theme.success;
        let warning = theme.warning;
        let danger = theme.danger;

        let close_view = cx.entity();
        let header = h_flex()
            .items_start()
            .justify_between()
            .gap_3()
            .child(
                v_flex()
                    .gap_0p5()
                    .child(self.header_title(foreground, success))
                    .child(self.header_subline(muted)),
            )
            .child(
                div()
                    .id("agent-spawn-sheet-close")
                    .px_1()
                    .text_color(muted)
                    .child(SharedString::new_static("×"))
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        cx.stop_propagation();
                        close_view.update(cx, |this, cx| {
                            this.close();
                            cx.notify();
                        });
                    }),
            );

        let body = match &self.phase {
            SheetPhase::Idle => {
                self.render_idle(window, cx, foreground, muted, border, primary, warning)
            }
            SheetPhase::Launching => self.render_launching(cx, primary, muted),
            SheetPhase::Streaming { id } => {
                let id = id.clone();
                self.render_streaming(cx, id, foreground, muted, border, primary, danger)
            }
        };

        v_flex()
            .id("agent-spawn-sheet")
            .absolute()
            .top_5()
            .right_5()
            .bottom_5()
            .w(px(540.0))
            .p_4()
            .gap_3()
            .bg(secondary)
            .border_1()
            .border_color(border)
            .rounded_md()
            // Stop propagation so clicks inside the sheet don't bubble
            // out to whatever route is rendered behind it (e.g. the
            // graph canvas drag-pan handlers on Home).
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(header)
            .child(body)
            .into_any_element()
    }
}

impl AgentSpawnSheet {
    fn header_title(&self, foreground: gpui::Hsla, success: gpui::Hsla) -> gpui::Div {
        match &self.phase {
            SheetPhase::Idle => div()
                .text_color(foreground)
                .child(SharedString::new_static("Launch agent")),
            SheetPhase::Launching => div()
                .text_color(foreground)
                .child(SharedString::new_static("Launching agent…")),
            SheetPhase::Streaming { id } => h_flex()
                .gap_2()
                .child(
                    div()
                        .text_color(foreground)
                        .child(SharedString::from(id.to_string())),
                )
                .child(
                    div()
                        .text_color(success)
                        .child(SharedString::new_static("● running")),
                ),
        }
    }

    fn header_subline(&self, muted: gpui::Hsla) -> gpui::Div {
        let copy: SharedString = match &self.phase {
            SheetPhase::Idle => SharedString::new_static(
                "Pick a profile · write a prompt · ⏎ doesn't submit (multi-line).",
            ),
            SheetPhase::Launching => SharedString::new_static("waiting on /api/agents/launch …"),
            SheetPhase::Streaming { .. } => {
                SharedString::new_static("log tail · refresh manually for now (auto-tail is v2)")
            }
        };
        div().text_color(muted).text_xs().child(copy)
    }

    #[allow(clippy::too_many_arguments)]
    fn render_idle(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
        foreground: gpui::Hsla,
        muted: gpui::Hsla,
        border: gpui::Hsla,
        primary: gpui::Hsla,
        warning: gpui::Hsla,
    ) -> gpui::AnyElement {
        let state = self.state.read(cx);
        let profiles: Vec<AgentProfileEntry> = state
            .agent_profiles
            .as_ref()
            .map(|r| r.profiles.clone())
            .unwrap_or_default();

        let view = cx.entity();

        // Profile picker column. Empty state surfaces a muted hint so
        // a fresh `crabcc serve` (which hasn't registered any profiles
        // yet) doesn't render a confusing blank pane.
        let mut picker = v_flex().gap_1().w(px(220.0)).child(
            div()
                .text_color(muted)
                .text_xs()
                .child(SharedString::new_static("PROFILE")),
        );
        if profiles.is_empty() {
            picker = picker.child(div().text_color(muted).text_xs().child(
                SharedString::new_static("no profiles loaded · using server default"),
            ));
        }
        for entry in &profiles {
            let id = entry.id.clone();
            let is_selected = self.selected_profile.as_deref() == Some(id.as_ref());
            let row_view = view.clone();
            let id_for_row = id.clone();
            picker =
                picker.child(
                    v_flex()
                        .id(SharedString::from(format!("agent-profile-{id}")))
                        .px_2()
                        .py_1()
                        .gap_0p5()
                        .border_l_2()
                        .border_color(if is_selected { primary } else { border })
                        .rounded_md()
                        .text_color(if is_selected { foreground } else { muted })
                        .child(
                            div()
                                .text_color(if is_selected { foreground } else { muted })
                                .child(SharedString::from(id.to_string())),
                        )
                        .child(
                            div().text_color(muted).text_xs().child(SharedString::from(
                                entry
                                    .description
                                    .as_ref()
                                    .map(|d| d.to_string())
                                    .unwrap_or_else(|| "—".into()),
                            )),
                        )
                        .child(div().text_color(muted).text_xs().child(SharedString::from(
                            format!(
                                "model: {}",
                                entry.model.as_ref().map(|m| m.as_ref()).unwrap_or("—")
                            ),
                        )))
                        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                            cx.stop_propagation();
                            let pid = id_for_row.clone();
                            row_view.update(cx, |this, cx| {
                                this.select_profile(pid);
                                cx.notify();
                            });
                        }),
                );
        }

        // Prompt textarea + submit row. Multi-line input — Enter inserts
        // newline; the visible Launch button is the only submit path.
        let submit_disabled = self.prompt_text.trim().is_empty();
        let submit_color = if submit_disabled { muted } else { primary };
        let submit_view = cx.entity();
        // Wrapper border brightens to `primary` while focused — same
        // focus-indicator pattern as the other route filter inputs
        // (#415, #416). Sheet's prompt input lives in `render_idle`,
        // so the focus check happens here.
        let prompt_focused = self
            .prompt_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        let prompt_border = if prompt_focused { primary } else { border };
        let prompt_field = div()
            .flex_1()
            .border_1()
            .border_color(prompt_border)
            .rounded_md()
            .px_2()
            .py_2()
            .min_h(px(180.0))
            .child(Input::new(&self.prompt_input).appearance(false));
        let submit_btn = div()
            .id("agent-spawn-sheet-launch")
            .px_3()
            .py_1()
            .border_1()
            .border_color(submit_color)
            .rounded_md()
            .text_color(submit_color)
            .child(SharedString::new_static("Launch"))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                cx.stop_propagation();
                submit_view.update(cx, |this, cx| this.submit(cx));
            });
        let prompt_col = v_flex()
            .flex_1()
            .gap_2()
            .child(
                div()
                    .text_color(muted)
                    .text_xs()
                    .child(SharedString::new_static("PROMPT")),
            )
            .child(prompt_field)
            .child(h_flex().justify_end().gap_2().child(submit_btn));

        let body = h_flex()
            .items_start()
            .gap_4()
            .child(picker)
            .child(prompt_col);

        // #306 closed the server-side gap — picking a profile now
        // forwards as the launch request's `profile` field. Pre-#306
        // servers silently ignore the field; warning kept off the
        // happy path.
        let _ = warning;
        v_flex().gap_3().child(body).into_any_element()
    }

    fn render_launching(
        &self,
        _cx: &mut Context<Self>,
        primary: gpui::Hsla,
        muted: gpui::Hsla,
    ) -> gpui::AnyElement {
        // Compact placeholder — most launches resolve in <1s, so a
        // structured "spinner" doesn't earn its keep here. Once the
        // result lands, the observer in `new` morphs the phase.
        v_flex()
            .gap_2()
            .child(
                div()
                    .px_2()
                    .py_1()
                    .border_1()
                    .border_color(primary)
                    .rounded_md()
                    .text_color(primary)
                    .child(SharedString::new_static("● spawning subprocess")),
            )
            .child(
                div()
                    .text_color(muted)
                    .text_xs()
                    .child(SharedString::new_static(
                        "if this hangs, check `crabcc serve` is reachable on 127.0.0.1:7878",
                    )),
            )
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_streaming(
        &self,
        cx: &mut Context<Self>,
        id: SharedString,
        foreground: gpui::Hsla,
        muted: gpui::Hsla,
        border: gpui::Hsla,
        primary: gpui::Hsla,
        danger: gpui::Hsla,
    ) -> gpui::AnyElement {
        let state = self.state.read(cx);
        // Render the log only if the most-recent fetch is for *this*
        // agent — guards against a stale tail from a prior selection
        // bleeding into the sheet.
        let log_text: SharedString = match state.agent_log.as_ref() {
            Some(log) if log.id == id => match &log.result {
                Ok(payload) => {
                    let body = payload.body.as_str();
                    let trimmed = body
                        .lines()
                        .rev()
                        .take(64)
                        .collect::<Vec<&str>>()
                        .into_iter()
                        .rev()
                        .collect::<Vec<&str>>()
                        .join("\n");
                    if trimmed.is_empty() {
                        SharedString::new_static("(no output yet)")
                    } else {
                        SharedString::from(trimmed)
                    }
                }
                Err(e) => SharedString::from(format!("log fetch failed: {e}")),
            },
            _ => SharedString::new_static("(refresh to load log)"),
        };

        // Read-only prompt preview so the user can confirm what was
        // submitted. Truncated visually by max-h; the full string is
        // already kept in `state.agents[id].prompt_preview` once SSE
        // catches up.
        let prompt_preview = div()
            .px_2()
            .py_1()
            .border_1()
            .border_color(border)
            .rounded_md()
            .text_color(muted)
            .text_xs()
            .max_h(px(80.0))
            .overflow_hidden()
            .child(SharedString::from(self.prompt_text.clone()));

        let log_block = div()
            .id("agent-spawn-sheet-log")
            .flex_1()
            .min_h(px(160.0))
            .px_2()
            .py_2()
            .border_1()
            .border_color(border)
            .rounded_md()
            .bg(gpui::black().opacity(0.35))
            .text_color(foreground)
            .text_xs()
            .overflow_y_scroll()
            .child(log_text);

        let refresh_view = cx.entity();
        let refresh_btn = div()
            .id("agent-spawn-sheet-refresh")
            .px_2()
            .py_1()
            .border_1()
            .border_color(border)
            .rounded_md()
            .text_color(muted)
            .child(SharedString::new_static("Refresh log"))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                cx.stop_propagation();
                refresh_view.update(cx, |this, cx| {
                    this.refresh_log(cx);
                    cx.notify();
                });
            });

        let kill_view = cx.entity();
        let kill_btn = div()
            .id("agent-spawn-sheet-kill")
            .px_2()
            .py_1()
            .border_1()
            .border_color(danger)
            .rounded_md()
            .text_color(danger)
            .child(SharedString::new_static("Kill"))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                cx.stop_propagation();
                kill_view.update(cx, |this, cx| this.kill(cx));
            });

        let detach_view = cx.entity();
        let detach_btn = div()
            .id("agent-spawn-sheet-detach")
            .px_2()
            .py_1()
            .border_1()
            .border_color(border)
            .rounded_md()
            .text_color(muted)
            .child(SharedString::new_static("Detach"))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                cx.stop_propagation();
                detach_view.update(cx, |this, cx| this.detach(cx));
            });

        let open_view = cx.entity();
        let open_btn = div()
            .id("agent-spawn-sheet-open-in-agents")
            .px_2()
            .py_1()
            .border_1()
            .border_color(primary)
            .rounded_md()
            .text_color(primary)
            .child(SharedString::new_static("Open in Agents route"))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                cx.stop_propagation();
                open_view.update(cx, |this, cx| this.open_in_agents(cx));
            });

        v_flex()
            .gap_3()
            .flex_1()
            .child(prompt_preview)
            .child(log_block)
            .child(
                h_flex()
                    .gap_2()
                    .child(refresh_btn)
                    .child(kill_btn)
                    .child(detach_btn)
                    .child(open_btn),
            )
            .into_any_element()
    }
}

/// Spawn an async loop that re-fetches the agent log every
/// [`LOG_TAIL_INTERVAL_MS`] while the sheet stays in `Streaming`
/// with the matching id. Exits cleanly on phase change (Detach /
/// Kill / Open in Agents) and on entity drop — gpui weakly captures
/// the entity, so a closed sheet doesn't keep the loop alive.
fn spawn_auto_tail(cx: &mut Context<AgentSpawnSheet>, id: SharedString) {
    use std::time::Duration;
    let interval = Duration::from_millis(LOG_TAIL_INTERVAL_MS);
    cx.spawn(async move |this, cx| {
        loop {
            cx.background_executor().timer(interval).await;
            // Bail if the entity has been dropped (window closed,
            // sheet rebuilt, etc.). `update`'s Result is `Err` once
            // the entity is gone.
            let still_streaming = match this.update(cx, |sheet, cx| match &sheet.phase {
                SheetPhase::Streaming { id: cur } if cur == &id => {
                    sheet.state.read(cx).submit_agent_log(id.clone(), 0);
                    true
                }
                _ => false,
            }) {
                Ok(v) => v,
                Err(_) => return,
            };
            if !still_streaming {
                return;
            }
        }
    })
    .detach();
}
