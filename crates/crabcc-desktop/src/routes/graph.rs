//! Relations graph viewer — A.5 (Milestone 2) + A.5.1 click-to-select +
//! A.5.2 wheel zoom + A.5.3 drag-to-pan and reset-view.
//!
//! A `gpui::canvas` view that paints the seed-graph from
//! `/api/seed-graph`:
//!
//!   * Edges are stroked thin lines (PathBuilder::stroke).
//!   * Nodes are filled quads with full corner_radii (≈ circles).
//!   * Click selects the nearest node within a fixed-pixel hit
//!     radius; an absolute-positioned overlay in the top-right
//!     shows the selection's id, degree, and a few neighbours.
//!   * Wheel zooms around the canvas centre (linear factor in
//!     [MIN_ZOOM, MAX_ZOOM]).
//!   * Press-and-drag pans the visible plane in unit coords. A
//!     mouse-up that hasn't crossed the drag threshold falls
//!     through to click-selection; a drag swallows the click.
//!   * The header has a "Reset view" affordance whenever the view
//!     is not at the identity (zoom == 1, pan == 0, 0).
//!
//! Layout runs once per `GraphSnapshot` identity (size of the node
//! set used as a cheap fingerprint). Resizing the window doesn't
//! re-layout — positions are stored in unit coords and scaled into
//! the live canvas bounds at paint time.
//!
//! Hit-test trick: paint stashes the latest canvas `Bounds<Pixels>`
//! on a `Mutex<Option<Bounds>>` shared with the click / drag
//! handlers. Both run on gpui's main thread sequentially, so
//! contention is nil; the mutex just gets the type-checker out of
//! the way of cross-closure sharing.

use std::collections::HashSet;
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

const MIN_ZOOM: f32 = 0.5;
const MAX_ZOOM: f32 = 4.0;
/// Sensitivity in `zoom_factor = exp(SCROLL_K * dy_px)`. 0.005 means
/// a 100-pixel scroll roughly halves or doubles the zoom — feels
/// natural on Apple Magic Mouse / trackpad without overshooting.
const SCROLL_K: f32 = 0.005;

/// Movement threshold (Manhattan, in pixels) below which a press-and-
/// release is treated as a click rather than a pan. 4px tolerates
/// jitter on Apple trackpads while staying tight enough that an
/// intentional pan registers immediately.
const DRAG_THRESHOLD_PX: f32 = 4.0;

#[derive(Clone, Copy)]
struct DragState {
    /// Window-coord cursor position when the press started.
    start_win: gpui::Point<Pixels>,
    /// View pan at the moment the press started — drag updates pan
    /// relative to this so the drag adds to the pre-drag offset.
    start_pan: (f32, f32),
    /// True once movement has crossed `DRAG_THRESHOLD_PX`. Once set
    /// the press is committed to drag-pan and the mouse-up does not
    /// trigger click-selection.
    moved: bool,
}

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
    /// Linear zoom factor around the canvas centre. 1.0 is the
    /// identity transform; values above 1 spread nodes out (zoom in),
    /// below 1 pack them tighter. Node visual radius stays constant —
    /// only positions transform, matching the d3 / scatter-plot
    /// convention.
    zoom: f32,
    /// Pan offset in unit (post-zoom) coords. Added to the visible
    /// position after the zoom transform; (0, 0) is the identity. The
    /// units are the same as the laid-out positions — so a pan of
    /// (0.1, 0) shifts everything 10% of the canvas to the right
    /// regardless of zoom level.
    pan: (f32, f32),
    /// `Some(_)` while the left button is held inside the canvas;
    /// `None` otherwise.
    drag: Option<DragState>,
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
            zoom: 1.0,
            pan: (0.0, 0.0),
            drag: None,
        }
    }

    fn adjust_zoom(&mut self, dy_px: f32) {
        // Exponential mapping — natural for zoom (a single scroll click
        // multiplies / divides instead of adding). Negative dy = scroll
        // down = zoom out (matches macOS/Linux convention).
        let factor = (SCROLL_K * dy_px).exp();
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
    }

    fn reset_view(&mut self) {
        self.zoom = 1.0;
        self.pan = (0.0, 0.0);
    }

    fn at_identity(&self) -> bool {
        (self.zoom - 1.0).abs() <= f32::EPSILON
            && self.pan.0.abs() <= f32::EPSILON
            && self.pan.1.abs() <= f32::EPSILON
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
    /// position the laid-out node table uses, inverting the zoom +
    /// pan transform along the way.
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
        // Invert paint's visible→unit transform:
        //   visible = 0.5 + (orig - 0.5) * zoom + pan
        // ⇒ orig = 0.5 + (visible - pan - 0.5) / zoom.
        let visible_x = local_x / w;
        let visible_y = local_y / h;
        let u_x = 0.5 + (visible_x - self.pan.0 - 0.5) / self.zoom;
        let u_y = 0.5 + (visible_y - self.pan.1 - 0.5) / self.zoom;
        // Hit radius is fixed in pixels (node markers don't scale with
        // zoom), but in unit space — which is what we compare in — it
        // shrinks by `zoom` because the inverse transform divides by
        // zoom along each axis.
        let r_px = NODE_RADIUS + HIT_PADDING_PX;
        let rx_u = r_px / (w * self.zoom);
        let ry_u = r_px / (h * self.zoom);
        let rx2 = rx_u * rx_u;
        let ry2 = ry_u * ry_u;
        let mut best: Option<(usize, f32)> = None;
        for (i, &(px_u, py_u)) in layout.positions.iter().enumerate() {
            let dx = px_u - u_x;
            let dy = py_u - u_y;
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

    /// Press start: record drag origin. `moved` stays false until
    /// movement crosses the threshold, at which point selection-on-
    /// release is suppressed for this gesture.
    fn drag_start(&mut self, win_pos: gpui::Point<Pixels>) {
        self.drag = Some(DragState {
            start_win: win_pos,
            start_pan: self.pan,
            moved: false,
        });
    }

    /// Active drag tick. Returns `true` if pan was updated (the
    /// caller uses this to decide whether to `cx.notify()`).
    fn drag_update(&mut self, win_pos: gpui::Point<Pixels>) -> bool {
        let Some(drag) = self.drag else {
            return false;
        };
        let bounds = match self.last_bounds.lock().ok().and_then(|g| *g) {
            Some(b) => b,
            None => return false,
        };
        let w = f32::from(bounds.size.width);
        let h = f32::from(bounds.size.height);
        if w <= 0.0 || h <= 0.0 {
            return false;
        }
        let dx_px = f32::from(win_pos.x - drag.start_win.x);
        let dy_px = f32::from(win_pos.y - drag.start_win.y);
        // Below the threshold, treat the gesture as still possibly a
        // click — don't move pan, don't commit `moved`.
        if !drag.moved && dx_px.abs() + dy_px.abs() < DRAG_THRESHOLD_PX {
            return false;
        }
        // Convert pixel delta to unit (canvas-relative) delta. Pan is
        // applied after the zoom transform, so the same drag distance
        // in pixels always shifts the view the same number of pixels
        // at any zoom level — feels right under the hand.
        let new_pan = (drag.start_pan.0 + dx_px / w, drag.start_pan.1 + dy_px / h);
        self.drag = Some(DragState {
            moved: true,
            ..drag
        });
        self.pan = new_pan;
        true
    }

    /// Press release. Returns `Some(win_pos)` if this should be
    /// treated as a click (drag never crossed the threshold). Returns
    /// `None` for drags or if no press was tracked.
    fn drag_end(&mut self, win_pos: gpui::Point<Pixels>) -> Option<gpui::Point<Pixels>> {
        let drag = self.drag.take()?;
        if drag.moved {
            None
        } else {
            Some(win_pos)
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
                // Pre-compute the neighbour set once so the per-node
                // paint loop can do an O(1) lookup. Cheap — graphs in
                // this view rarely top a few hundred nodes, and the
                // set is rebuilt only on selection change anyway.
                let neighbours: HashSet<usize> = match selected_idx {
                    Some(i) => self.neighbours(i).into_iter().collect(),
                    None => HashSet::new(),
                };
                let zoom = self.zoom;
                let pan = self.pan;
                let bounds_share = self.last_bounds.clone();

                let canvas_el = canvas(
                    move |_bounds, _, _| (),
                    move |bounds: Bounds<Pixels>, _, window, _| {
                        // Stash bounds for the click / drag handlers.
                        // The lock is contention-free in practice —
                        // only the gpui main thread touches it.
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
                            &neighbours,
                            zoom,
                            pan,
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
                            .child(div().text_color(muted).child(SharedString::from(format!(
                                "{degree} edge{}",
                                if degree == 1 { "" } else { "s" }
                            ))))
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
                // the press / move / release set + scroll wheel.
                // `relative()` positions the absolute overlay against
                // this container's bounds.
                let entity_for_down = cx.entity();
                let entity_for_move = cx.entity();
                let entity_for_up = cx.entity();
                let entity_for_up_out = cx.entity();
                let entity_for_scroll = cx.entity();
                div()
                    .id("relations-graph")
                    .relative()
                    .size_full()
                    .child(canvas_el)
                    .child(info_panel)
                    .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                        let pos = event.position;
                        entity_for_down.update(cx, |this, cx| {
                            this.drag_start(pos);
                            cx.notify();
                        });
                    })
                    .on_mouse_move(move |event, _, cx| {
                        // Only relevant while the left button is held.
                        // Other moves (hover) would force needless
                        // notifies.
                        if event.pressed_button != Some(MouseButton::Left) {
                            return;
                        }
                        let pos = event.position;
                        entity_for_move.update(cx, |this, cx| {
                            if this.drag_update(pos) {
                                cx.notify();
                            }
                        });
                    })
                    .on_mouse_up(MouseButton::Left, move |event, _, cx| {
                        let pos = event.position;
                        entity_for_up.update(cx, |this, cx| {
                            if let Some(click_pos) = this.drag_end(pos) {
                                this.handle_click(click_pos);
                            }
                            cx.notify();
                        });
                    })
                    .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
                        // Mouse released outside the canvas — drop
                        // the gesture without firing a click.
                        entity_for_up_out.update(cx, |this, cx| {
                            this.drag = None;
                            cx.notify();
                        });
                    })
                    .on_scroll_wheel(move |event, _, cx| {
                        // 16 px is a reasonable line-height proxy. macOS
                        // trackpad scroll deltas are already pixel-precise
                        // and ignore the proxy; legacy mouse wheels (line-
                        // based) get scaled by it.
                        let dy = f32::from(event.delta.pixel_delta(px(16.0)).y);
                        if dy != 0.0 {
                            entity_for_scroll.update(cx, |this, cx| {
                                this.adjust_zoom(dy);
                                cx.notify();
                            });
                        }
                    })
                    .into_any_element()
            }
        };

        // ── Header ─────────────────────────────────────────────────
        let mut header = h_flex().gap_3().child(
            div()
                .text_sm()
                .child(SharedString::new_static("Relations graph")),
        );
        // Node + edge counts. Pulled from the cached layout (set on
        // the most recent `ensure_layout`) — same data the canvas is
        // painting from, so the numbers always match the picture.
        if let Some(l) = self.layout.as_ref() {
            let nodes = l.positions.len();
            let edges = l.edge_indices.len();
            header = header.child(div().text_color(muted).child(SharedString::from(format!(
                "· {nodes} node{} · {edges} edge{}",
                if nodes == 1 { "" } else { "s" },
                if edges == 1 { "" } else { "s" },
            ))));
        }
        if (self.zoom - 1.0).abs() > f32::EPSILON {
            header = header.child(div().text_color(muted).child(SharedString::from(format!(
                "· {:.0}% zoom",
                self.zoom * 100.0
            ))));
        }
        if !self.at_identity() {
            let entity_for_reset = cx.entity();
            header = header.child(
                div()
                    .id("graph-reset-view")
                    .ml_2()
                    .px_2()
                    .py_0p5()
                    .border_1()
                    .border_color(border)
                    .rounded_md()
                    .text_color(primary)
                    .child(SharedString::new_static("Reset view"))
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        entity_for_reset.update(cx, |this, cx| {
                            this.reset_view();
                            cx.notify();
                        });
                    }),
            );
        }
        if self.selected.is_some() {
            header = header.child(
                div()
                    .text_color(muted)
                    .child(SharedString::new_static("· click outside to clear")),
            );
        }

        v_flex()
            .size_full()
            .min_h(px(360.0))
            .p_3()
            .gap_2()
            .border_1()
            .border_color(border)
            .rounded_md()
            .bg(cx.theme().secondary)
            .child(header)
            .child(body)
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_graph(
    bounds: Bounds<Pixels>,
    layout: &Layout,
    edge_color: Hsla,
    node_color: Hsla,
    highlight_color: Hsla,
    selected: Option<usize>,
    neighbours: &HashSet<usize>,
    zoom: f32,
    pan: (f32, f32),
    window: &mut Window,
) {
    let ox = f32::from(bounds.origin.x);
    let oy = f32::from(bounds.origin.y);
    let w = f32::from(bounds.size.width);
    let h = f32::from(bounds.size.height);

    // Zoom + pan transform: visible = 0.5 + (orig - 0.5) * zoom + pan.
    // Inverse lives in `hit_test`. Nodes that fall outside [0, 1]
    // visible range simply paint outside the canvas and get clipped —
    // gpui doesn't error on out-of-bounds paint calls.
    let to_px = |(ux, uy): (f32, f32)| {
        let evx = 0.5 + (ux - 0.5) * zoom + pan.0;
        let evy = 0.5 + (uy - 0.5) * zoom + pan.1;
        point(px(ox + evx * w), px(oy + evy * h))
    };

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

    // Nodes. Three render tiers:
    //   * selected     — larger radius + full highlight colour
    //   * neighbour    — base radius + dimmed highlight colour
    //   * other        — base radius + base node colour
    // Edges incident to the selection are already stroked in the
    // brighter highlight above, so the eye traces the connection
    // ring naturally without further wiring.
    let r = NODE_RADIUS;
    let r_sel = NODE_RADIUS + 2.0;
    let neighbour_color = with_alpha(highlight_color, 0.55);
    for (i, pos) in layout.positions.iter().enumerate() {
        let is_selected = selected == Some(i);
        let is_neighbour = !is_selected && neighbours.contains(&i);
        let radius = if is_selected { r_sel } else { r };
        let color = if is_selected {
            highlight_color
        } else if is_neighbour {
            neighbour_color
        } else {
            node_color
        };
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
