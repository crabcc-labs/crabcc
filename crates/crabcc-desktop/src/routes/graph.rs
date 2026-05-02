//! Relations graph viewer — A.5 (Milestone 2) + A.5.1 click-to-select.
//!
//! A `gpui::canvas` view that paints the seed-graph from
//! `/api/seed-graph`:
//!
//!   * Edges are stroked thin lines (PathBuilder::stroke).
//!   * Nodes are filled quads with full corner_radii (≈ circles).
//!   * Clicking near a node selects it; an absolute-positioned overlay
//!     in the top-right shows the node's id, degree, and a few
//!     connected neighbours.
//!
//! Layout runs once per `GraphSnapshot` identity (size of the node
//! set used as a cheap fingerprint). Resizing the window doesn't
//! re-layout — positions are stored in unit coords and scaled into
//! the live canvas bounds at paint time.
//!
//! Hit-test trick: paint stashes the latest canvas `Bounds<Pixels>`
//! on a `Mutex<Option<Bounds>>` shared with the click handler. Both
//! run on gpui's main thread sequentially, so contention is nil; the
//! mutex just gets the type-checker out of the way of cross-closure
//! sharing. Click reads the bounds, converts the window-relative
//! event position to unit coords, and walks the position list once.
//!
//! Zoom + pan land in A.5.2.

use std::sync::{Arc, Mutex};

use gpui::{
    canvas, div, point, prelude::*, px, Bounds, Context, Entity, Hsla, IntoElement, MouseButton,
    PathBuilder, Pixels, Render, SharedString, Window,
};
use gpui_component::{h_flex, v_flex, ActiveTheme};

use crate::api::types::GraphSnapshot;
use crate::graph_layout::{self, Layout};
use crate::state::AppState;

const NODE_RADIUS: f32 = 4.0;
/// Click tolerance — anything within (NODE_RADIUS + this) of a node's
/// centre counts as a hit. Keeps the small node visual hittable
/// without making clicks ambiguous in dense clusters.
const HIT_PADDING_PX: f32 = 4.0;
const EDGE_WIDTH: f32 = 1.0;

pub struct GraphView {
    state: Entity<AppState>,
    layout: Option<Layout>,
    layout_fingerprint: usize,
    /// Index into `layout.positions` of the currently-selected node,
    /// or `None`. Set on click; cleared by clicking outside any node.
    selected: Option<usize>,
    /// Latest canvas bounds, written by paint and read by the click
    /// handler. Both fire on the main thread sequentially — the mutex
    /// is just a type-system convenience for sharing across the two
    /// closures gpui takes ownership of.
    last_bounds: Arc<Mutex<Option<Bounds<Pixels>>>>,
}

impl GraphView {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self {
            state,
            layout: None,
            layout_fingerprint: 0,
            selected: None,
            last_bounds: Arc::new(Mutex::new(None)),
        }
    }

    fn ensure_layout(&mut self, snapshot: &GraphSnapshot) -> &Layout {
        let fp = snapshot.nodes.len() ^ (snapshot.edges.len() << 16);
        if self.layout.is_none() || self.layout_fingerprint != fp {
            self.layout = Some(graph_layout::run(snapshot));
            self.layout_fingerprint = fp;
            // Snapshot identity changed — drop the now-stale selection.
            self.selected = None;
        }
        self.layout.as_ref().expect("set above")
    }

    /// Convert a window-relative click position into the unit-coord
    /// position the laid-out node table uses.
    fn hit_test(&self, win_pos: gpui::Point<Pixels>) -> Option<usize> {
        let bounds = self.last_bounds.lock().ok()?.as_ref().copied()?;
        let layout = self.layout.as_ref()?;
        let local_x = f32::from(win_pos.x - bounds.origin.x);
        let local_y = f32::from(win_pos.y - bounds.origin.y);
        let w = f32::from(bounds.size.width);
        let h = f32::from(bounds.size.height);
        if w <= 0.0 || h <= 0.0 {
            return None;
        }
        let u_x = local_x / w;
        let u_y = local_y / h;
        // Pixel-space radius for the hit test — convert NODE_RADIUS +
        // padding back to unit coords using the live canvas size, so
        // the click target stays NODE_RADIUS-px in pixels regardless
        // of zoom level (and once zoom lands).
        let r_px = NODE_RADIUS + HIT_PADDING_PX;
        let rx_u = r_px / w;
        let ry_u = r_px / h;
        let rx2 = rx_u * rx_u;
        let ry2 = ry_u * ry_u;
        // Find the closest hit node within the ellipsoid radius. The
        // canvas may be non-square so x and y radii differ.
        let mut best: Option<(usize, f32)> = None;
        for (i, &(px_u, py_u)) in layout.positions.iter().enumerate() {
            let dx = px_u - u_x;
            let dy = py_u - u_y;
            // Normalised distance in unit coords; <1 means inside.
            let nd = dx * dx / rx2 + dy * dy / ry2;
            if nd <= 1.0 && best.map(|(_, b)| nd < b).unwrap_or(true) {
                best = Some((i, nd));
            }
        }
        best.map(|(i, _)| i)
    }

    fn handle_click(&mut self, win_pos: gpui::Point<Pixels>) {
        match self.hit_test(win_pos) {
            Some(i) => self.selected = Some(i),
            None => self.selected = None,
        }
    }

    fn neighbours(&self, idx: usize) -> Vec<usize> {
        let Some(layout) = self.layout.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for &(a, b) in &layout.edge_indices {
            if a == idx {
                out.push(b);
            } else if b == idx {
                out.push(a);
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

impl Render for GraphView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let primary = cx.theme().primary;
        let secondary = cx.theme().secondary;
        let foreground = cx.theme().foreground;

        let snapshot_opt = {
            let state = self.state.read(cx);
            state.graph.clone()
        };

        let body: gpui::AnyElement = match snapshot_opt {
            None => div()
                .text_color(muted)
                .child(SharedString::new_static("loading graph…"))
                .into_any_element(),
            Some(snapshot) => {
                let layout = self.ensure_layout(&snapshot).clone();
                let node_color = primary;
                let edge_color = with_alpha(muted, 0.45);
                let highlight_color = cx.theme().success;
                let selected_idx = self.selected;
                let bounds_share = self.last_bounds.clone();

                let canvas_el = canvas(
                    move |_bounds, _, _| (),
                    move |bounds: Bounds<Pixels>, _, window, _| {
                        // Stash bounds for the click handler. The lock
                        // is contention-free in practice — only the
                        // gpui main thread touches it.
                        if let Ok(mut guard) = bounds_share.lock() {
                            *guard = Some(bounds);
                        }
                        paint_graph(
                            bounds,
                            &layout,
                            edge_color,
                            node_color,
                            highlight_color,
                            selected_idx,
                            window,
                        );
                    },
                )
                .size_full();

                // Optional overlay panel (top-right) for the selected
                // node's id + degree + first-N neighbours. Lives in
                // the same outer element so it scrolls / resizes with
                // the canvas.
                let info_panel: gpui::AnyElement = match self.selected {
                    None => div().into_any_element(),
                    Some(idx) => {
                        let snapshot_ref = self.state.read(cx).graph.as_ref();
                        let label = snapshot_ref
                            .and_then(|g| g.nodes.get(idx))
                            .map(|n| n.id.clone())
                            .unwrap_or_else(|| "—".into());
                        let neighbours: Vec<String> = self
                            .neighbours(idx)
                            .into_iter()
                            .take(8)
                            .filter_map(|n| {
                                snapshot_ref
                                    .and_then(|g| g.nodes.get(n))
                                    .map(|n| n.id.clone())
                            })
                            .collect();
                        let degree = self
                            .layout
                            .as_ref()
                            .map(|l| {
                                l.edge_indices
                                    .iter()
                                    .filter(|&&(a, b)| a == idx || b == idx)
                                    .count()
                            })
                            .unwrap_or(0);

                        v_flex()
                            .absolute()
                            .top_2()
                            .right_2()
                            .min_w(px(220.0))
                            .max_w(px(280.0))
                            .p_2()
                            .gap_1()
                            .bg(secondary)
                            .border_1()
                            .border_color(border)
                            .rounded_md()
                            .child(
                                div()
                                    .text_color(foreground)
                                    .child(SharedString::from(label)),
                            )
                            .child(
                                div().text_color(muted).child(SharedString::from(format!(
                                    "{degree} edge{}",
                                    if degree == 1 { "" } else { "s" }
                                ))),
                            )
                            .children(neighbours.into_iter().map(|n| {
                                div()
                                    .text_color(muted)
                                    .child(SharedString::from(format!("· {n}")))
                                    .into_any_element()
                            }))
                            .into_any_element()
                    }
                };

                // Wrap canvas in an interactive container that owns
                // the click. `relative()` positions the absolute
                // overlay against this container's bounds.
                let entity_for_click = cx.entity();
                div()
                    .id("relations-graph")
                    .relative()
                    .size_full()
                    .child(canvas_el)
                    .child(info_panel)
                    .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                        let pos = event.position;
                        entity_for_click.update(cx, |this, cx| {
                            this.handle_click(pos);
                            cx.notify();
                        });
                    })
                    .into_any_element()
            }
        };

        v_flex()
            .size_full()
            .min_h(px(360.0))
            .p_3()
            .gap_2()
            .border_1()
            .border_color(border)
            .rounded_md()
            .bg(cx.theme().secondary)
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .child(SharedString::new_static("Relations graph")),
                    )
                    .children(self.selected.map(|_| {
                        div()
                            .text_color(muted)
                            .child(SharedString::new_static("· click outside to clear"))
                    })),
            )
            .child(body)
    }
}

fn paint_graph(
    bounds: Bounds<Pixels>,
    layout: &Layout,
    edge_color: Hsla,
    node_color: Hsla,
    highlight_color: Hsla,
    selected: Option<usize>,
    window: &mut Window,
) {
    let ox = f32::from(bounds.origin.x);
    let oy = f32::from(bounds.origin.y);
    let w = f32::from(bounds.size.width);
    let h = f32::from(bounds.size.height);

    let to_px = |(ux, uy): (f32, f32)| point(px(ox + ux * w), px(oy + uy * h));

    // Edges first. Edges incident to the selected node use the
    // highlight colour at higher alpha — gives the selection a halo
    // without redrawing edges twice.
    for &(a, b) in &layout.edge_indices {
        if let (Some(p1), Some(p2)) = (layout.positions.get(a), layout.positions.get(b)) {
            let mut pb = PathBuilder::stroke(px(EDGE_WIDTH));
            pb.move_to(to_px(*p1));
            pb.line_to(to_px(*p2));
            if let Ok(path) = pb.build() {
                let on_selected = selected.is_some_and(|s| s == a || s == b);
                let color = if on_selected {
                    with_alpha(highlight_color, 0.85)
                } else {
                    edge_color
                };
                window.paint_path(path, color);
            }
        }
    }

    // Nodes. Selected node renders larger + in the highlight colour.
    let r = NODE_RADIUS;
    let r_sel = NODE_RADIUS + 2.0;
    for (i, pos) in layout.positions.iter().enumerate() {
        let is_selected = selected == Some(i);
        let radius = if is_selected { r_sel } else { r };
        let color = if is_selected { highlight_color } else { node_color };
        let centre = to_px(*pos);
        let node_bounds = Bounds {
            origin: point(centre.x - px(radius), centre.y - px(radius)),
            size: gpui::size(px(radius * 2.0), px(radius * 2.0)),
        };
        window.paint_quad(
            gpui::fill(node_bounds, color).corner_radii(gpui::Corners::all(px(radius))),
        );
    }
}

fn with_alpha(c: Hsla, a: f32) -> Hsla {
    Hsla { a, ..c }
}
