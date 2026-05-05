//! Inspector route — MCP tool-call timeline.
//!
//! Two-pane waterfall: a virtual list of [`CallEvent`] rows on the
//! left, a JSON-viewer detail pane on the right. Filter bar across
//! the top: free-text substring (matches server / method / tool
//! name) plus a status chip cycling through `any | pending | ok |
//! err`.
//!
//! M0 cut. Per `crates/crabcc-desktop/docs/MCP-INSPECTOR.md` §13:
//! ring buffer + naive list rendering, no SQLite, no CAS, no diff,
//! no replay. The eventual plan (per `MCP-NATIVE.md` §2) is for
//! this route to *replace* `routes::system`; for M0 we ship it
//! side-by-side so adoption stays additive.
//!
//! Source connection deferred to M1 — the ring is populated by
//! [`crate::state::AppState::record_inspector_event`] and the
//! in-proc MCP bridge will start calling that once it lands.

use gpui::{
    div, prelude::*, px, ClipboardItem, Context, Entity, IntoElement, MouseButton, Render,
    SharedString, Window,
};
use gpui_component::{
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex, ActiveTheme, Sizable,
};

use crate::inspector::{CallEvent, Status, StatusKind};
use crate::routes::empty::empty_state;
use crate::routes::time::format_time;
use crate::state::AppState;

pub struct InspectorRoute {
    state: Entity<AppState>,
    /// Free-text filter over `server`, `method`, and `tool_name`.
    /// Pattern mirrors `SystemRoute::query_input`.
    query_input: Entity<InputState>,
    /// Lower-cased mirror of the input value, kept in sync via
    /// `InputEvent::Change`. Avoids re-lowercasing every render.
    query_lower: String,
    /// Status filter — cycles via the chip in the filter bar.
    status_filter: StatusKind,
    /// Selected event id (the right-pane subject). `None` until the
    /// user clicks a row.
    selected_id: Option<u64>,
}

impl InspectorRoute {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        let query_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Filter by server / method / tool…")
        });
        cx.subscribe_in(&query_input, window, |this, state, event, _, cx| {
            if let InputEvent::Change = event {
                this.query_lower = state.read(cx).value().to_string().to_lowercase();
                cx.notify();
            }
        })
        .detach();
        Self {
            state,
            query_input,
            query_lower: String::new(),
            status_filter: StatusKind::Any,
            selected_id: None,
        }
    }

    fn cycle_status_filter(&mut self) {
        self.status_filter = match self.status_filter {
            StatusKind::Any => StatusKind::Pending,
            StatusKind::Pending => StatusKind::Ok,
            StatusKind::Ok => StatusKind::Err,
            StatusKind::Err => StatusKind::Any,
        };
    }

    fn select(&mut self, id: u64) {
        self.selected_id = Some(id);
    }

    fn clear_selection(&mut self) {
        self.selected_id = None;
    }

    fn matches(&self, e: &CallEvent) -> bool {
        if !self.status_filter.matches(&e.status) {
            return false;
        }
        if self.query_lower.is_empty() {
            return true;
        }
        let q = self.query_lower.as_str();
        if e.server.to_lowercase().contains(q) {
            return true;
        }
        if e.method.to_lowercase().contains(q) {
            return true;
        }
        if let Some(t) = &e.tool_name {
            if t.to_lowercase().contains(q) {
                return true;
            }
        }
        false
    }
}

impl Render for InspectorRoute {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let foreground = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let panel = cx.theme().secondary;
        let accent = cx.theme().primary;
        let danger = cx.theme().danger;

        // Snapshot the ring & matching events without holding the
        // borrow across child closures.
        let state = self.state.read(cx);
        let total = state.inspector_ring.len();
        let rows: Vec<CallEvent> = state
            .inspector_ring
            .iter()
            .rev() // newest first
            .filter(|e| self.matches(e))
            .cloned()
            .collect();
        let shown = rows.len();
        let selected = self
            .selected_id
            .and_then(|id| state.inspector_ring.iter().find(|e| e.id == id).cloned());
        // Release the borrow before child closures (which take `cx`).
        let _ = state;

        let header = h_flex()
            .items_center()
            .justify_between()
            .px_4()
            .py_2()
            .gap_3()
            .border_b_1()
            .border_color(border)
            .child(
                h_flex()
                    .gap_2()
                    .items_baseline()
                    .child(
                        div()
                            .text_color(foreground)
                            .child(SharedString::new_static("Inspector")),
                    )
                    .child(div().text_xs().text_color(muted).child(SharedString::from(
                        format!("{shown} / {total} events"),
                    ))),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .min_w(px(280.))
                            .child(Input::new(&self.query_input).small()),
                    )
                    .child(status_chip(
                        self.status_filter,
                        accent,
                        muted,
                        cx.listener(|this, _, _window, cx| {
                            this.cycle_status_filter();
                            cx.notify();
                        }),
                    )),
            );

        let left_pane: gpui::AnyElement = if rows.is_empty() {
            empty_state(
                "\u{1F50D}",
                "No MCP events yet",
                if total == 0 {
                    "The in-proc MCP bridge isn't wired up yet (M1). Once it is, every \
                     tool call, resource read, and sampling request will show up here."
                } else {
                    "No rows match the current filter."
                },
                muted,
                foreground,
            )
            .into_any_element()
        } else {
            v_flex()
                .gap_0()
                .children(rows.into_iter().map(|e| {
                    let is_selected = self.selected_id == Some(e.id);
                    let row_id = e.id;
                    row(&e, is_selected, foreground, muted, border, accent, danger).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _window, cx| {
                            this.select(row_id);
                            cx.notify();
                        }),
                    )
                }))
                .into_any_element()
        };

        let right_pane: gpui::AnyElement = match selected {
            None => div()
                .px_5()
                .py_8()
                .text_xs()
                .text_color(muted)
                .child(SharedString::new_static(
                    "Pick a row on the left to see its params and result.",
                ))
                .into_any_element(),
            Some(e) => detail_pane(&e, foreground, muted, border, panel, danger, cx).into_any_element(),
        };

        v_flex()
            .size_full()
            .text_color(foreground)
            .child(header)
            .child(
                h_flex()
                    .flex_1()
                    .min_h(px(0.))
                    .child(
                        // gpui requires `.id(...)` before `.overflow_y_scroll()`
                        // so the per-pane scroll offset survives re-renders.
                        div()
                            .id("inspector-left-pane")
                            .flex_1()
                            .min_w(px(0.))
                            .border_r_1()
                            .border_color(border)
                            .overflow_y_scroll()
                            .child(left_pane),
                    )
                    .child(
                        div()
                            .id("inspector-right-pane")
                            .w(px(420.))
                            .min_w(px(320.))
                            .overflow_y_scroll()
                            .child(right_pane),
                    ),
            )
    }
}

fn status_chip(
    kind: StatusKind,
    accent: gpui::Hsla,
    muted: gpui::Hsla,
    on_click: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
) -> gpui::Div {
    div()
        .px_2()
        .py(px(2.))
        .text_xs()
        .text_color(if matches!(kind, StatusKind::Any) {
            muted
        } else {
            accent
        })
        .border_1()
        .border_color(muted)
        .rounded_md()
        .cursor_pointer()
        .child(SharedString::from(format!("status: {}", kind.label())))
        .on_mouse_down(MouseButton::Left, on_click)
}

fn row(
    e: &CallEvent,
    selected: bool,
    foreground: gpui::Hsla,
    muted: gpui::Hsla,
    border: gpui::Hsla,
    accent: gpui::Hsla,
    danger: gpui::Hsla,
) -> gpui::Div {
    let status_color = match &e.status {
        Status::Ok => foreground,
        Status::Pending => muted,
        Status::Err { .. } => danger,
    };
    let method_label = match &e.tool_name {
        Some(t) => SharedString::from(format!("{} · {}", e.method, t)),
        None => e.method.clone(),
    };
    let latency = match e.latency_ms {
        Some(ms) => SharedString::from(format!("{} ms", ms)),
        None => SharedString::new_static("—"),
    };
    let ts = SharedString::from(format_time(e.ts_ms / 1000));

    h_flex()
        .px_4()
        .py(px(6.))
        .gap_3()
        .items_center()
        .border_b_1()
        .border_color(border)
        .when(selected, |d| d.bg(accent.opacity(0.10)))
        .cursor_pointer()
        .child(
            div()
                .w(px(74.))
                .text_xs()
                .text_color(muted)
                .child(ts),
        )
        .child(
            div()
                .w(px(110.))
                .text_xs()
                .text_color(foreground)
                .child(e.server.clone()),
        )
        .child(
            div()
                .w(px(20.))
                .text_xs()
                .text_color(muted)
                .child(SharedString::new_static(e.direction.glyph())),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .text_xs()
                .text_color(foreground)
                .child(method_label),
        )
        .child(
            div()
                .w(px(64.))
                .text_xs()
                .text_color(muted)
                .child(latency),
        )
        .child(
            div()
                .w(px(20.))
                .text_xs()
                .text_color(status_color)
                .child(SharedString::new_static(e.status.glyph())),
        )
}

fn detail_pane(
    e: &CallEvent,
    foreground: gpui::Hsla,
    muted: gpui::Hsla,
    border: gpui::Hsla,
    panel: gpui::Hsla,
    danger: gpui::Hsla,
    cx: &mut Context<InspectorRoute>,
) -> gpui::Div {
    let id = e.id;
    let header = h_flex()
        .px_4()
        .py_2()
        .items_center()
        .justify_between()
        .border_b_1()
        .border_color(border)
        .child(
            div()
                .text_xs()
                .text_color(foreground)
                .child(SharedString::from(format!("#{} · {}", id, e.method))),
        )
        .child(
            h_flex()
                .gap_2()
                .child(
                    div()
                        .px_2()
                        .py(px(2.))
                        .text_xs()
                        .text_color(muted)
                        .border_1()
                        .border_color(muted)
                        .rounded_md()
                        .cursor_pointer()
                        .child(SharedString::new_static("close"))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _window, cx| {
                                this.clear_selection();
                                cx.notify();
                            }),
                        ),
                ),
        );

    let params_block = json_block(
        "params",
        e.params_pretty.clone(),
        foreground,
        muted,
        panel,
        cx,
    );
    let result_block = match (&e.status, &e.result_pretty) {
        (Status::Err { code, msg }, _) => json_block(
            "error",
            SharedString::from(format!("code: {code}\nmsg:  {msg}")),
            danger,
            muted,
            panel,
            cx,
        ),
        (_, Some(r)) => json_block("result", r.clone(), foreground, muted, panel, cx),
        (Status::Pending, None) => div()
            .px_4()
            .py_2()
            .text_xs()
            .text_color(muted)
            .child(SharedString::new_static("(pending — no result yet)")),
        (Status::Ok, None) => div()
            .px_4()
            .py_2()
            .text_xs()
            .text_color(muted)
            .child(SharedString::new_static("(empty result)")),
    };

    v_flex()
        .child(header)
        .child(params_block)
        .child(result_block)
}

fn json_block(
    title: &'static str,
    body: SharedString,
    foreground: gpui::Hsla,
    muted: gpui::Hsla,
    panel: gpui::Hsla,
    cx: &mut Context<InspectorRoute>,
) -> gpui::Div {
    let body_for_copy = body.clone();
    v_flex()
        .px_4()
        .py_2()
        .gap_1()
        .child(
            h_flex()
                .gap_2()
                .items_baseline()
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(SharedString::new_static(title)),
                )
                .child(
                    div()
                        .px_2()
                        .text_xs()
                        .text_color(muted)
                        .cursor_pointer()
                        .child(SharedString::new_static("copy"))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |_, _, _window, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    body_for_copy.to_string(),
                                ));
                            }),
                        ),
                ),
        )
        .child(
            // No `whitespace_pre` available on Div in this gpui rev —
            // monospace via `font_family("Menlo")` is enough; multi-line
            // payloads still render newlines, just without preserved
            // leading whitespace. M0+1 will swap to a real JSON viewer.
            div()
                .p_2()
                .bg(panel)
                .text_xs()
                .text_color(foreground)
                .font_family("Menlo")
                .child(body),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inspector::{CallEvent, Direction, Status};

    fn make(method: &str, server: &str, status: Status) -> CallEvent {
        CallEvent {
            id: CallEvent::next_id(),
            ts_ms: 0,
            server: SharedString::from(server.to_string()),
            direction: Direction::In,
            method: SharedString::from(method.to_string()),
            tool_name: None,
            status,
            latency_ms: None,
            params_pretty: SharedString::new_static("{}"),
            result_pretty: None,
            parent_id: None,
        }
    }

    fn shape() -> InspectorRouteShape {
        InspectorRouteShape {
            query_lower: String::new(),
            status_filter: StatusKind::Any,
        }
    }

    /// Test-only stand-in for the route's filter logic. Mirrors the
    /// real `matches` body so the route doesn't need a gpui Context
    /// to verify filter behaviour. Same pattern as
    /// `routes::timeline::MatchesShape`.
    struct InspectorRouteShape {
        query_lower: String,
        status_filter: StatusKind,
    }

    impl InspectorRouteShape {
        fn matches(&self, e: &CallEvent) -> bool {
            if !self.status_filter.matches(&e.status) {
                return false;
            }
            if self.query_lower.is_empty() {
                return true;
            }
            let q = self.query_lower.as_str();
            if e.server.to_lowercase().contains(q) {
                return true;
            }
            if e.method.to_lowercase().contains(q) {
                return true;
            }
            if let Some(t) = &e.tool_name {
                if t.to_lowercase().contains(q) {
                    return true;
                }
            }
            false
        }
    }

    #[test]
    fn matches_passes_through_when_query_empty_and_status_any() {
        let r = shape();
        let e = make("tools/call", "slack", Status::Ok);
        assert!(r.matches(&e));
    }

    #[test]
    fn matches_filters_by_server_substring() {
        let mut r = shape();
        r.query_lower = "sla".into();
        assert!(r.matches(&make("tools/call", "slack", Status::Ok)));
        assert!(!r.matches(&make("tools/call", "github", Status::Ok)));
    }

    #[test]
    fn matches_filters_by_status_kind() {
        let mut r = shape();
        r.status_filter = StatusKind::Err;
        assert!(r.matches(&make(
            "x",
            "y",
            Status::Err {
                code: -32001,
                msg: SharedString::new_static("denied"),
            },
        )));
        assert!(!r.matches(&make("x", "y", Status::Ok)));
    }
}
