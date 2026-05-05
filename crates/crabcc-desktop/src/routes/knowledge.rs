//! Knowledge route — memory drawer browser + ingest form.
//!
//! Reads from `AppState::memory_recent` (refreshed every 10s by the
//! memory poller). The form at the top POSTs to `/api/memory/ingest`
//! and pushes a follow-up `MemoryRefresh` so the new drawer appears
//! immediately rather than waiting up to 10s for the next poll.
//!
//! A second TextInput below the ingest form filters the visible
//! drawers as the user types (id / wing/room / body preview,
//! case-insensitive). Mirrors the Agents-route filter pattern.

use gpui::{
    div, prelude::*, px, Context, Entity, Focusable, Hsla, IntoElement, MouseButton, Render,
    SharedString, Window,
};
use gpui_component::{
    h_flex,
    input::{Input, InputEvent, InputState},
    tooltip::Tooltip,
    v_flex, ActiveTheme,
};

use crate::api::types::{MemoryDrawer, MemoryIngestRequest};
use crate::routes::empty::empty_state;
use crate::state::AppState;

const VISIBLE_DRAWERS: usize = 50;
const INGEST_SOURCE: &str = "desktop:ingest";

pub struct KnowledgeRoute {
    state: Entity<AppState>,
    ingest_input: Entity<InputState>,
    /// Live mirror of the input's text — read on submit so the click
    /// handler doesn't need to crack open the entity again.
    pending_text: String,
    /// Filter input — narrows the visible drawer list.
    filter_input: Entity<InputState>,
    /// Lower-cased mirror of the filter input value, kept in sync via
    /// `InputEvent::Change`. Avoids re-lowercasing on every render.
    filter_lower: String,
    /// Active wing-pin filter — set by clicking a drawer's wing badge,
    /// cleared by the active-pill `×` in the header. ANDed with
    /// `filter_lower` so a user can drill in: pin a wing, then refine
    /// by substring within it.
    wing_pin: Option<SharedString>,
}

impl KnowledgeRoute {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        let ingest_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Ingest a note (text)…"));
        cx.subscribe_in(
            &ingest_input,
            window,
            |this, state, event, window, cx| match event {
                InputEvent::Change => {
                    this.pending_text = state.read(cx).value().to_string();
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => {
                    this.submit(window, cx);
                }
                _ => {}
            },
        )
        .detach();
        let filter_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Filter by id / wing / room / body…")
        });
        cx.subscribe_in(&filter_input, window, |this, state, event, _, cx| {
            if let InputEvent::Change = event {
                this.filter_lower = state.read(cx).value().to_string().to_lowercase();
                cx.notify();
            }
        })
        .detach();
        Self {
            state,
            ingest_input,
            pending_text: String::new(),
            filter_input,
            filter_lower: String::new(),
            wing_pin: None,
        }
    }

    fn drawer_matches(&self, d: &MemoryDrawer) -> bool {
        // Wing pin first — exact match, fast reject. Reduces the
        // substring-search workload when a wing is pinned.
        if let Some(pin) = self.wing_pin.as_ref() {
            if &d.wing != pin {
                return false;
            }
        }
        if self.filter_lower.is_empty() {
            return true;
        }
        let q = self.filter_lower.as_str();
        if d.id.to_string().contains(q) {
            return true;
        }
        if d.wing.to_lowercase().contains(q) {
            return true;
        }
        if let Some(room) = d.room.as_deref() {
            if room.to_lowercase().contains(q) {
                return true;
            }
        }
        d.body_preview.to_lowercase().contains(q)
    }

    fn pin_wing(&mut self, wing: SharedString) {
        // Click on the active wing toggles it off — saves the user
        // hunting for the header `×` for a casual narrow-then-clear.
        if self.wing_pin.as_deref() == Some(wing.as_ref()) {
            self.wing_pin = None;
        } else {
            self.wing_pin = Some(wing);
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.pending_text.trim();
        if text.is_empty() {
            return;
        }
        let req = MemoryIngestRequest {
            text: Some(text.to_string()),
            source: Some(INGEST_SOURCE.to_string()),
            ..Default::default()
        };
        // Fire-and-forget — `submit_ingest` spawns its own thread; the
        // result lands back through the worker channel as
        // `AppEvent::MemoryIngestResult` (+ a follow-up
        // `MemoryRefresh` on success). Clear the input synchronously
        // so a user ingesting several drawers in a row doesn't have
        // to manually backspace each time. If submit_ingest fails,
        // the status line below the form surfaces the error and the
        // user can re-enter — losing one accidental empty buffer is
        // a fair trade for the multi-ingest workflow.
        self.state.read(cx).submit_ingest(req);
        self.pending_text.clear();
        self.ingest_input.update(cx, |st, sub_cx| {
            st.set_value("", window, sub_cx);
        });
        cx.notify();
    }
}

impl Render for KnowledgeRoute {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Cross-route nav handoff (e.g. K-Graph → Knowledge): a prior
        // render staged a filter string. Mirror it into both
        // `filter_lower` and the `InputState` text so the field shows
        // what's actually filtering the list. One-shot — subsequent
        // renders don't re-apply, so the user can clear it manually.
        let pending_filter = self
            .state
            .update(cx, |s, _| s.take_pending_knowledge_filter());
        if let Some(value) = pending_filter {
            let lower = value.to_lowercase();
            self.filter_lower = lower;
            self.filter_input.update(cx, |st, sub_cx| {
                st.set_value(value, window, sub_cx);
            });
        }

        let state = self.state.read(cx);
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let secondary = cx.theme().secondary;
        let primary = cx.theme().primary;
        let success = cx.theme().success;
        let danger = cx.theme().danger;

        let status_line: gpui::AnyElement = match state.last_ingest.as_ref() {
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

        let submit_disabled = self.pending_text.trim().is_empty();
        let submit_color = if submit_disabled { muted } else { primary };
        // Capture an entity handle for the click handler to read +
        // call `submit_ingest` through. Cloning is cheap.
        let route_entity = cx.entity();
        let foreground = cx.theme().foreground;
        // Only earn the cursor + hover treatment when there's
        // actually something to submit — empty prompt should look
        // and act inert (same pattern as the spawn-sheet Launch
        // button #426 / Commands runnable chips #413).
        let mut submit_btn = div()
            .id("memory-ingest-submit")
            .px_3()
            .py_1()
            .border_1()
            .border_color(submit_color)
            .rounded_md()
            .text_color(submit_color);
        if !submit_disabled {
            submit_btn = submit_btn
                .cursor_pointer()
                .hover(move |s| s.bg(primary).text_color(foreground))
                .tooltip(|window, cx| {
                    Tooltip::new("Ingest into the memory backend").build(window, cx)
                });
        }
        let submit_btn = submit_btn
            .child(SharedString::new_static("Ingest"))
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                route_entity.update(cx, |this, cx| this.submit(window, cx));
            });

        let form = h_flex()
            .gap_2()
            .child({
                // Wrapper border brightens to `primary` while
                // focused — same focus-indicator pattern as the
                // other route filters.
                let ingest_focused = self
                    .ingest_input
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window);
                let ingest_border = if ingest_focused { primary } else { border };
                div()
                    .flex_1()
                    .border_1()
                    .border_color(ingest_border)
                    .rounded_md()
                    .px_2()
                    .py_1()
                    .child(Input::new(&self.ingest_input).appearance(false))
            })
            .child(submit_btn);

        let filter_focused = self
            .filter_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        let filter_border = if filter_focused { primary } else { border };
        let filter_field = div()
            .flex_1()
            .border_1()
            .border_color(filter_border)
            .rounded_md()
            .px_2()
            .py_1()
            .child(Input::new(&self.filter_input).appearance(false));

        // Active wing-pin pill — only renders when a wing is pinned.
        // Acts as the canonical clear-affordance (clicking the pinned
        // wing badge in a row also toggles it off, but the header pill
        // is the place a user looks when narrowing feels stuck).
        let pin_pill: gpui::AnyElement = match self.wing_pin.as_ref() {
            None => div().into_any_element(),
            Some(wing) => {
                let entity_for_clear = cx.entity();
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_color(muted)
                            .child(SharedString::new_static("wing pinned:")),
                    )
                    .child(
                        div()
                            .id("knowledge-wing-pin-clear")
                            .px_2()
                            .py_0p5()
                            .border_1()
                            .border_color(primary)
                            .rounded_md()
                            .text_color(primary)
                            .cursor_pointer()
                            .hover(move |s| s.bg(secondary))
                            .tooltip(|window, cx| Tooltip::new("Clear wing pin").build(window, cx))
                            .child(SharedString::from(format!("{wing} \u{00D7}")))
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                entity_for_clear.update(cx, |this, cx| {
                                    this.wing_pin = None;
                                    cx.notify();
                                });
                            }),
                    )
                    .into_any_element()
            }
        };

        // Manual refresh — same wire shape as the 10s poller, just
        // user-driven. Useful when an ingest landed via CLI / MCP /
        // Telegram and the user wants the new drawer immediately
        // instead of waiting up to 10s for the next tick.
        let refresh_state = self.state.clone();
        let refresh_btn_color = primary;
        let refresh_btn = div()
            .id("knowledge-refresh-btn")
            .px_2()
            .py_0p5()
            .border_1()
            .border_color(border)
            .rounded_md()
            .text_color(refresh_btn_color)
            .text_xs()
            .child(SharedString::new_static("Refresh"))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                refresh_state.read(cx).submit_memory_refresh();
            });
        let header = v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_3()
                    .child(div().text_lg().child(SharedString::new_static("Knowledge")))
                    .child(
                        div()
                            .flex_1()
                            .text_color(muted)
                            .child(SharedString::new_static(
                                "Drawers refresh every 10s · Enter or click Ingest to submit.",
                            )),
                    )
                    .child(refresh_btn),
            )
            .child(form)
            .child(status_line)
            .child(filter_field)
            .child(pin_pill);

        let body: gpui::AnyElement = match state.memory_recent.as_ref() {
            None => div()
                .text_color(muted)
                .min_h(px(60.0))
                .child(SharedString::new_static("loading drawers…"))
                .into_any_element(),
            Some(resp) if !resp.present => empty_state(
                "\u{26A0}",
                "Memory backend not initialised",
                "Run `crabcc memory init` to create `.crabcc/memory.db` for this repo.",
                muted,
                cx.theme().foreground,
            )
            .into_any_element(),
            Some(resp) if resp.drawers.is_empty() => empty_state(
                "\u{25CC}",
                "No drawers yet",
                "Run `crabcc memory ingest` from the CLI to add new ones.",
                muted,
                cx.theme().foreground,
            )
            .into_any_element(),
            Some(resp) => {
                let total = resp.drawers.len();
                let visible: Vec<&MemoryDrawer> = resp
                    .drawers
                    .iter()
                    .filter(|d| self.drawer_matches(d))
                    .take(VISIBLE_DRAWERS)
                    .collect();
                let visible_count = visible.len();
                let count_line = SharedString::from(if self.filter_lower.is_empty() {
                    format!("{total} drawers · cursor {}", resp.cursor)
                } else {
                    format!("{visible_count} of {total} match · cursor {}", resp.cursor)
                });
                // Wing distribution — aggregates over the *unfiltered*
                // drawer set so the user sees the full repo shape
                // even when narrowed; otherwise narrowing on a
                // specific wing trivially shows that wing as 100%
                // and hides the comparison context. Capped at the
                // top 5 wings; smaller wings collapse into "+N more".
                let wing_summary = wing_summary_line(&resp.drawers, 5);
                let entity = cx.entity();
                let pinned_wing = self.wing_pin.clone();
                let list: gpui::AnyElement = if visible.is_empty() {
                    empty_state(
                        "\u{1F50D}",
                        "No drawers match the filter",
                        &format!(
                            "Nothing matches \u{201C}{}\u{201D} — try a shorter query.",
                            self.filter_lower
                        ),
                        muted,
                        cx.theme().foreground,
                    )
                    .into_any_element()
                } else {
                    v_flex()
                        .gap_1()
                        .children(
                            visible
                                .into_iter()
                                .map(|d| {
                                    let wing_pinned =
                                        pinned_wing.as_deref() == Some(d.wing.as_str());
                                    drawer_row(
                                        d,
                                        muted,
                                        border,
                                        secondary,
                                        primary,
                                        wing_pinned,
                                        entity.clone(),
                                        self.state.clone(),
                                    )
                                    .into_any_element()
                                })
                                .collect::<Vec<_>>(),
                        )
                        .into_any_element()
                };
                let wing_line: gpui::AnyElement = match wing_summary {
                    Some(text) => div()
                        .text_color(muted)
                        .child(SharedString::from(text))
                        .into_any_element(),
                    None => div().into_any_element(),
                };
                v_flex()
                    .gap_2()
                    .child(div().text_color(muted).child(count_line))
                    .child(wing_line)
                    .child(list)
                    .into_any_element()
            }
        };

        v_flex()
            .size_full()
            .px_5()
            .py_4()
            .gap_3()
            .child(header)
            .child(body)
    }
}

#[allow(clippy::too_many_arguments)]
fn drawer_row(
    d: &MemoryDrawer,
    muted: Hsla,
    border: Hsla,
    badge_bg: Hsla,
    primary: Hsla,
    wing_pinned: bool,
    entity: Entity<KnowledgeRoute>,
    state: Entity<AppState>,
) -> gpui::Div {
    let location: SharedString = match d.room.as_deref() {
        Some(room) if !room.is_empty() => format!("{}/{}", d.wing, room).into(),
        _ => d.wing.clone(),
    };
    // Click target id needs to be unique per row — `gpui` requires
    // stateful elements to declare an id, and it must not collide
    // across the children list within a single render. Drawer ids
    // are unique within the response so suffixing with `d.id` is
    // sufficient. `SharedString` rather than `&'static str` so
    // `format!` works.
    let badge_id = SharedString::from(format!("knowledge-wing-{}-{}", d.id, d.wing));
    let pin_wing = d.wing.clone();
    let badge_border = if wing_pinned { primary } else { badge_bg };

    // "→ K-Graph" cross-link — navigates to K-Graph with this drawer
    // pre-selected. Mirrors the K-Graph → Knowledge link from #383
    // closing the pair: from a drawer's body view, dive into the
    // canvas to see who points at it / what it points at. Drawer
    // source_id matches MemoryGraphNode.id 1:1 (server-side mapping).
    let nav_id = d.source_id.clone();
    let nav_state = state.clone();
    let nav_btn_id = SharedString::from(format!("knowledge-to-kgraph-{}", d.id));
    let kgraph_link = div()
        .id(nav_btn_id)
        .px_2()
        .py_0p5()
        .border_1()
        .border_color(border)
        .rounded_md()
        .text_color(muted)
        .cursor_pointer()
        .hover(move |s| s.border_color(primary).text_color(primary))
        .tooltip(|window, cx| Tooltip::new("Open K-Graph at this drawer").build(window, cx))
        .child(SharedString::new_static("\u{2192} K-Graph"))
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            let id = nav_id.clone();
            nav_state.update(cx, |s, cx| {
                s.navigate_to_kgraph_with_selection(id);
                cx.notify();
            });
        });

    v_flex()
        .gap_1()
        .py_2()
        .border_b_1()
        .border_color(border)
        .child(
            h_flex()
                .gap_3()
                // Drawer id — fixed-width column for visual alignment.
                .child(
                    div()
                        .w(px(60.0))
                        .text_color(muted)
                        .child(SharedString::from(format!("#{}", d.id))),
                )
                // Wing/room badge — uses the secondary token as a
                // subtle pill background. Clicking pins the wing as
                // an extra filter; clicking the active wing toggles
                // the pin off.
                .child({
                    let wing_tooltip: SharedString = if wing_pinned {
                        SharedString::new_static("Unpin wing — show all wings")
                    } else {
                        SharedString::from(format!("Pin wing {} — narrows the drawer list", d.wing))
                    };
                    div()
                        .id(badge_id)
                        .px_2()
                        .py_0p5()
                        .bg(badge_bg)
                        .border_1()
                        .border_color(badge_border)
                        .rounded_md()
                        .cursor_pointer()
                        .hover(move |s| s.border_color(primary))
                        .tooltip(move |window, cx| {
                            Tooltip::new(wing_tooltip.clone()).build(window, cx)
                        })
                        .child(location)
                        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                            let wing = pin_wing.clone();
                            entity.update(cx, |this, cx| {
                                this.pin_wing(wing);
                                cx.notify();
                            });
                        })
                })
                .child(
                    div()
                        .flex_1()
                        .text_color(muted)
                        .child(SharedString::from(format_relative(d.created_at))),
                )
                .child(kgraph_link),
        )
        .child(SharedString::from(truncate(&d.body_preview, 220)))
}

/// Build the wing-distribution summary line — `"doc 5 · session 3 · fix 2"`.
/// Returns `None` when the input is empty or has only one distinct
/// wing (a single-wing drawer pile gives no useful distribution
/// information; the count line above already shows the total).
///
/// `cap` bounds the visible wings; any tail collapses into a `+N more`
/// suffix so the line stays scannable on a wide repo.
fn wing_summary_line(drawers: &[MemoryDrawer], cap: usize) -> Option<String> {
    if drawers.len() < 2 {
        return None;
    }
    // Tally by wing. Linear scan + small-vec — no HashMap (the wing
    // cardinality on a typical repo is < 10, so a Vec is fine and
    // keeps the iteration order deterministic for the sort below).
    // Tally key is `SharedString` so the `.clone()` per new wing is a
    // refcount bump, not a heap allocation.
    let mut tally: Vec<(SharedString, usize)> = Vec::with_capacity(8);
    for d in drawers {
        if let Some(slot) = tally.iter_mut().find(|(k, _)| k == &d.wing) {
            slot.1 += 1;
        } else {
            tally.push((d.wing.clone(), 1));
        }
    }
    if tally.len() < 2 {
        return None;
    }
    // Sort by count desc, then wing name asc for ties.
    tally.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.as_ref().cmp(b.0.as_ref())));
    let visible: Vec<String> = tally
        .iter()
        .take(cap)
        .map(|(w, n)| format!("{w} {n}"))
        .collect();
    let mut line = visible.join(" · ");
    if tally.len() > cap {
        let more = tally.len() - cap;
        line.push_str(&format!(" · +{more} more"));
    }
    Some(line)
}

/// "Ns ago" / "Nm ago" / "Nh ago" — coarse but readable for a
/// developer-facing memory tail. Computed against
/// `SystemTime::now()` so timezone is irrelevant.
fn format_relative(unix_seconds: i64) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(unix_seconds);
    let delta = (now - unix_seconds).max(0);
    if delta < 60 {
        format!("{delta}s ago")
    } else if delta < 3600 {
        format!("{}m ago", delta / 60)
    } else if delta < 86_400 {
        format!("{}h ago", delta / 3600)
    } else {
        format!("{}d ago", delta / 86_400)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_buckets() {
        // Computed against `now`, so we exercise the relative bucket
        // ladder by passing relative offsets.
        let now: i64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!(format_relative(now - 5).ends_with("s ago"));
        assert!(format_relative(now - 120).ends_with("m ago"));
        assert!(format_relative(now - 7200).ends_with("h ago"));
        assert!(format_relative(now - 200_000).ends_with("d ago"));
    }

    #[test]
    fn truncate_appends_ellipsis() {
        assert_eq!(truncate("abc", 10), "abc");
        assert_eq!(truncate("abcdef", 4), "abc…");
    }

    fn drawer(id: i64, wing: &str) -> MemoryDrawer {
        MemoryDrawer {
            id,
            wing: wing.into(),
            room: None,
            source_id: SharedString::new_static(""),
            body_preview: SharedString::new_static(""),
            created_at: 0,
        }
    }

    #[test]
    fn wing_summary_skips_single_wing() {
        // A pile that's all one wing has nothing useful to say —
        // the count line above already shows the total.
        let ds: Vec<MemoryDrawer> = (0..5).map(|i| drawer(i, "doc")).collect();
        assert_eq!(wing_summary_line(&ds, 5), None);
    }

    #[test]
    fn wing_summary_skips_empty() {
        assert_eq!(wing_summary_line(&[], 5), None);
    }

    #[test]
    fn wing_summary_orders_by_count_then_name() {
        let ds = vec![
            drawer(1, "doc"),
            drawer(2, "doc"),
            drawer(3, "doc"),
            drawer(4, "session"),
            drawer(5, "session"),
            drawer(6, "fix"),
        ];
        assert_eq!(
            wing_summary_line(&ds, 5),
            Some("doc 3 · session 2 · fix 1".to_string())
        );
    }

    #[test]
    fn wing_summary_collapses_tail_into_more() {
        let ds = vec![
            drawer(1, "doc"),
            drawer(2, "session"),
            drawer(3, "fix"),
            drawer(4, "thinking"),
            drawer(5, "memory"),
            drawer(6, "tail-1"),
            drawer(7, "tail-2"),
        ];
        let line = wing_summary_line(&ds, 3).unwrap();
        assert!(line.starts_with("doc 1 · "));
        assert!(line.contains(" · +4 more"));
    }
}
