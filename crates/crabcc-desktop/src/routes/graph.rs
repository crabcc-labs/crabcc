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
    canvas, div, point, prelude::*, px, Bounds, ClipboardItem, Context, Entity, Hsla, IntoElement,
    MouseButton, PathBuilder, Pixels, Render, SharedString, Window,
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

    /// Split the edges incident to `idx` into incoming (callers) and
    /// outgoing (callees). Wraps the pure helper `directed_edges_of`
    /// with the layout lookup so the free function stays unit-testable
    /// without touching `Entity<AppState>`.
    fn directed_edges(&self, idx: usize) -> (Vec<usize>, Vec<usize>) {
        match self.layout.as_ref() {
            Some(l) => directed_edges_of(&l.edge_indices, idx),
            None => (Vec::new(), Vec::new()),
        }
    }
}

/// Walk a directed edge list `(src, dst)` and split the edges incident
/// to `idx` into incoming (callers) and outgoing (callees). Both lists
/// are sorted + deduped so repeat edges between the same pair render
/// as a single row in the drawer.
///
/// Self-loops (a == b == idx) appear in BOTH lists by design, so the
/// caller can see they exist from either direction. Callers that don't
/// want self-loops should filter them out before invoking.
fn directed_edges_of(edges: &[(usize, usize)], idx: usize) -> (Vec<usize>, Vec<usize>) {
    let mut incoming = Vec::new();
    let mut outgoing = Vec::new();
    for &(a, b) in edges {
        if b == idx {
            incoming.push(a);
        }
        if a == idx {
            outgoing.push(b);
        }
    }
    incoming.sort_unstable();
    incoming.dedup();
    outgoing.sort_unstable();
    outgoing.dedup();
    (incoming, outgoing)
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

                // Right-side node-detail drawer (#296). Slides in (no
                // animation yet — first cut) when a node is selected.
                // Today the seed-graph response only carries `id` +
                // `depth`, so kind / file:line / signature / snippet
                // are deliberately absent and called out inline; the
                // server-side enrichment is tracked separately.
                let info_panel: gpui::AnyElement = match self.selected {
                    None => div().into_any_element(),
                    Some(idx) => render_node_drawer(
                        idx,
                        self.state.read(cx).graph.as_ref(),
                        self.directed_edges(idx),
                        cx.entity(),
                        secondary,
                        border,
                        muted,
                        foreground,
                        primary,
                    ),
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

/// How many incoming / outgoing rows to render before collapsing the
/// rest behind a "show all N →" affordance. 6 keeps the drawer from
/// dominating the canvas on hubs while still surfacing enough context
/// for the common case (most symbols have <6 callers).
const DRAWER_EDGE_VISIBLE: usize = 6;

#[allow(clippy::too_many_arguments)]
fn render_node_drawer(
    idx: usize,
    snapshot: Option<&GraphSnapshot>,
    directed: (Vec<usize>, Vec<usize>),
    view: Entity<GraphView>,
    secondary: Hsla,
    border: Hsla,
    muted: Hsla,
    foreground: Hsla,
    primary: Hsla,
) -> gpui::AnyElement {
    let (incoming, outgoing) = directed;
    let node = snapshot.and_then(|g| g.nodes.get(idx));
    let label = node.map(|n| n.id.clone()).unwrap_or_else(|| "—".into());
    let depth = node.map(|n| n.depth);
    // Server-side enrichment fields (#301). Pre-#301 servers omit
    // them — defaulted to None via `serde(default)`. The drawer
    // gracefully degrades: missing fields just don't render.
    let kind_str = node.and_then(|n| n.kind.clone());
    let file = node.and_then(|n| n.file.clone());
    let line = node.and_then(|n| n.line);
    let signature = node.and_then(|n| n.signature.clone());
    let degree = incoming.len() + outgoing.len();

    // Resolve neighbour indexes back to ids once. Costs O(degree) — the
    // drawer caps each list visually but keeps full lists in memory so
    // a future "show all" expansion doesn't need a re-fetch.
    let id_for = |n: usize| -> SharedString {
        snapshot
            .and_then(|g| g.nodes.get(n))
            .map(|n| SharedString::from(n.id.clone()))
            .unwrap_or_else(|| SharedString::new_static("—"))
    };

    let copy_label = label.clone();
    let copy_view = view.clone();
    let close_view = view.clone();

    // Kind badge — small inline chip in the kind's family colour.
    // Maps `function` / `struct` / etc. to the existing `op_color`-
    // adjacent palette so the drawer reads consistently with the
    // dashboard activity tile.
    let kind_chip: gpui::AnyElement = match kind_str.as_deref() {
        Some(k) => {
            let chip_color = kind_color(k, primary, muted);
            div()
                .px_2()
                .py_0p5()
                .border_1()
                .border_color(chip_color)
                .rounded_md()
                .text_color(chip_color)
                .text_xs()
                .child(SharedString::from(k.to_string()))
                .into_any_element()
        }
        None => div().into_any_element(),
    };

    // Subline: depth + edge count + (optional) file:line. Each part is
    // separated by `·` so a missing optional folds the row cleanly.
    let mut subline_parts: Vec<String> = Vec::with_capacity(3);
    if let Some(d) = depth {
        subline_parts.push(format!("depth {d}"));
    }
    subline_parts.push(format!(
        "{degree} edge{}",
        if degree == 1 { "" } else { "s" }
    ));
    if let (Some(f), Some(l)) = (file.as_deref(), line) {
        subline_parts.push(format!("{f}:{l}"));
    }
    let subline_text = subline_parts.join(" · ");

    // Header — symbol id (mono) + kind chip + meta line + close.
    let header = h_flex()
        .items_start()
        .justify_between()
        .gap_2()
        .child(
            v_flex()
                .gap_0p5()
                .child(
                    h_flex().gap_2().items_center().child(kind_chip).child(
                        div()
                            .text_color(foreground)
                            .child(SharedString::from(label.clone())),
                    ),
                )
                .child(
                    div()
                        .text_color(muted)
                        .text_xs()
                        .child(SharedString::from(subline_text)),
                ),
        )
        .child(
            div()
                .id("graph-drawer-close")
                .px_1()
                .text_color(muted)
                .child(SharedString::new_static("×"))
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    cx.stop_propagation();
                    close_view.update(cx, |this, cx| {
                        this.selected = None;
                        cx.notify();
                    });
                }),
        );

    // One edge sub-list. Caller picks Incoming vs Outgoing label and
    // passes the resolved indexes — we don't filter or reorder here.
    let edge_list = |title: &'static str, ids: &[usize]| -> gpui::Div {
        let total = ids.len();
        let visible = ids.iter().take(DRAWER_EDGE_VISIBLE).copied();
        let mut list = v_flex().gap_0p5().child(
            div()
                .text_color(muted)
                .text_xs()
                .child(SharedString::from(format!("{title} ({total})"))),
        );
        for n in visible {
            let row_view = view.clone();
            list = list.child(
                div()
                    .id(SharedString::from(format!("graph-drawer-edge-{title}-{n}")))
                    .pl_2()
                    .text_color(foreground)
                    .text_xs()
                    .child(id_for(n))
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        cx.stop_propagation();
                        row_view.update(cx, |this, cx| {
                            this.selected = Some(n);
                            cx.notify();
                        });
                    }),
            );
        }
        if total > DRAWER_EDGE_VISIBLE {
            let extra = total - DRAWER_EDGE_VISIBLE;
            list = list.child(
                div()
                    .pl_2()
                    .text_color(muted)
                    .text_xs()
                    .child(SharedString::from(format!("+ {extra} more"))),
            );
        }
        list
    };

    // Signature block. Renders only when the server enrichment
    // (#301) supplied one. Snippet (the brief's other section) still
    // requires source-file access the desktop client doesn't have —
    // tracked separately.
    let signature_block: gpui::AnyElement = match signature.as_deref() {
        Some(sig) => v_flex()
            .gap_1()
            .child(
                div()
                    .text_color(muted)
                    .text_xs()
                    .child(SharedString::new_static("SIGNATURE")),
            )
            .child(
                div()
                    .px_2()
                    .py_1()
                    .border_1()
                    .border_color(border)
                    .rounded_md()
                    .bg(gpui::black().opacity(0.35))
                    .text_color(foreground)
                    .text_xs()
                    .child(SharedString::from(sig.to_string())),
            )
            .into_any_element(),
        None => div().into_any_element(),
    };

    // Pre-#301 servers don't emit kind / file / line / signature —
    // the drawer falls back gracefully (sections that need a field
    // skip if the field is None), but a one-line reminder still
    // helps the user understand why the drawer is sparse on a stale
    // server. Renders only when *all four* enrichment fields are
    // missing (i.e. an old server).
    let server_gap_note: gpui::AnyElement =
        if kind_str.is_none() && file.is_none() && line.is_none() && signature.is_none() {
            div()
                .text_color(muted)
                .text_xs()
                .child(SharedString::new_static(
                    "running against a pre-#301 server — kind / file:line / signature unavailable",
                ))
                .into_any_element()
        } else {
            div().into_any_element()
        };

    let actions = h_flex().gap_2().child(
        div()
            .id("graph-drawer-copy-id")
            .px_2()
            .py_1()
            .border_1()
            .border_color(border)
            .rounded_md()
            .text_color(primary)
            .child(SharedString::new_static("Copy id"))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                cx.stop_propagation();
                let item = ClipboardItem::new_string(copy_label.clone());
                cx.write_to_clipboard(item);
                copy_view.update(cx, |_, cx| cx.notify());
            }),
    );

    v_flex()
        .id("graph-node-drawer")
        .absolute()
        .top_2()
        .right_2()
        .w(px(340.0))
        .max_h(px(560.0))
        .p_3()
        .gap_3()
        .bg(secondary)
        .border_1()
        .border_color(border)
        .rounded_md()
        // Stop propagation so clicks inside the drawer don't drive the
        // canvas drag-pan / click-to-select state machine. Without
        // this, mouse-up on a button at the right edge of the canvas
        // triggers `handle_click(pos)` which then clears the selection
        // because no node is under the drawer.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(header)
        .child(signature_block)
        .child(edge_list("Incoming", &incoming))
        .child(edge_list("Outgoing", &outgoing))
        .child(actions)
        .child(server_gap_note)
        .into_any_element()
}

/// Map a server-emitted kind string (`function` / `struct` / `enum` /
/// `trait` / `const` / `type` / `macro` / etc. — see `crabcc-viz`'s
/// `symbol_kind_str`) to a tone in the gpui-component theme. The
/// palette mirrors the dashboard's `op_color` so a user who's
/// internalised the activity-tile colours reads the same families
/// here.
fn kind_color(kind: &str, primary: Hsla, muted: Hsla) -> Hsla {
    match kind {
        // primary purple for fns + their kin, plus type-construction
        // shapes (struct / enum) — these dominate row volume in a
        // typical Rust repo and cluster well as one visual family.
        "function" | "method" | "macro" => primary,
        // muted for shapes the eye doesn't need to track first.
        "const" | "var" | "type" => muted,
        // class / interface / struct / enum / trait — keep them all
        // primary too for now; we can split if a user calls out the
        // need to distinguish them at a glance.
        "class" | "interface" | "struct" | "enum" | "trait" => primary,
        _ => muted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directed_edges_separates_in_and_out() {
        // 0 → 1, 0 → 2, 3 → 1, 1 → 4. From node 1's perspective:
        //   incoming = [0, 3], outgoing = [4].
        let edges = vec![(0, 1), (0, 2), (3, 1), (1, 4)];
        let (incoming, outgoing) = directed_edges_of(&edges, 1);
        assert_eq!(incoming, vec![0, 3]);
        assert_eq!(outgoing, vec![4]);
    }

    #[test]
    fn directed_edges_dedupes_parallel_edges() {
        // Two parallel 0 → 1 edges, plus one 0 → 1 reversed pair, get
        // collapsed to a single neighbour on each side.
        let edges = vec![(0, 1), (0, 1), (1, 0)];
        let (incoming, outgoing) = directed_edges_of(&edges, 0);
        assert_eq!(incoming, vec![1]);
        assert_eq!(outgoing, vec![1]);
    }

    #[test]
    fn directed_edges_handles_unrelated_index() {
        let edges = vec![(0, 1), (1, 2)];
        let (incoming, outgoing) = directed_edges_of(&edges, 9);
        assert!(incoming.is_empty());
        assert!(outgoing.is_empty());
    }

    #[test]
    fn directed_edges_self_loops_appear_both_sides() {
        // Self-loop should surface in both directions so the drawer
        // doesn't silently lose it. graph_layout strips self-loops at
        // ingest, but if the server ever sends one, this is the
        // contract the drawer presents.
        let edges = vec![(2, 2)];
        let (incoming, outgoing) = directed_edges_of(&edges, 2);
        assert_eq!(incoming, vec![2]);
        assert_eq!(outgoing, vec![2]);
    }
}
