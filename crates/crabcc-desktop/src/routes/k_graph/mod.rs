//! Knowledge graph view (#317). Renders the `/api/memory/graph`
//! response — drawers as nodes, cross-references (resolved
//! server-side from `web:<hash>` / `text:<hash>` / `doc:<n>` ids and
//! Obsidian-style `[[Title]]` matches) as edges.
//!
//! The brief asks for a Roam-like canvas distinct from the relations
//! graph. This route ships:
//!
//!   * a **force-directed canvas** at the top of the route — nodes
//!     are wing-coloured rounded-rect pills (NOT circles, the
//!     relations graph's primary), edges are thin solid lines, and
//!     the selected node carries a foreground-coloured ring. Click a
//!     node to select; click empty canvas to deselect. The top-N
//!     highest-degree drawers (degree ≥ 3, capped at 8) get their
//!     title painted under the pill so the dense knots are
//!     identifiable without scrolling the list below. The canvas
//!     supports **wheel-zoom** + **drag-to-pan** (same shape as
//!     `routes::graph`); a "Reset view" affordance appears in the
//!     header whenever the view is off the identity transform.
//!   * the **wing-grouped list** below the canvas — same data, easier
//!     to scan at scale.
//!   * a **right-rail Drawer Detail** panel on the active selection
//!     (incoming + outgoing edge lists, `via` annotation).
//!
//! State is stored on `AppState::memory_graph` (lazy fetch on first
//! render via `submit_memory_graph`; manual refresh button re-runs
//! the same path). Errors land on `AppState::memory_graph_error` and
//! render inline.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use gpui::{
    canvas, div, point, prelude::*, px, App, Bounds, ClipboardItem, Context, Entity, Hsla,
    IntoElement, MouseButton, PathBuilder, Pixels, Render, SharedString, TextRun, Window,
};
use gpui_component::{h_flex, tooltip::Tooltip, v_flex, ActiveTheme};

use crate::api::types::{GraphEdge, GraphNode, GraphSnapshot, MemoryGraphEdge, MemoryGraphNode};
use crate::graph_layout::{self, Layout};
use crate::routes::empty::empty_state;
use crate::state::AppState;

/// Pill dimensions for memory-graph nodes — wider than tall so the
/// shape reads as a "pill" / "card". Distinct from the relations
/// graph's circles (5-7 px radius). Caps a comfortable density at
/// 200-300 nodes; beyond that the layout collapses into overlap and
/// pan/zoom becomes load-bearing (not in v1).
const PILL_WIDTH: f32 = 14.0;
const PILL_HEIGHT: f32 = 8.0;
/// Click tolerance — anything within this many pixels of a pill's
/// bounding box counts as a hit. Keeps the small pill hittable in
/// dense clusters without making clicks ambiguous.
const HIT_PADDING_PX: f32 = 4.0;
const EDGE_WIDTH: f32 = 1.0;
/// Canvas height. The shell wraps the body in `overflow_y_scroll`,
/// so the route can be taller than the window — keep the canvas
/// generous so the layout has room to breathe.
const CANVAS_HEIGHT: f32 = 380.0;

/// Cap on the rows shown in the per-section list. `recent_activity`
/// equivalent — keeps paint cost bounded under deep memory; a
/// follow-up search/filter input lifts this if needed.
const SECTION_ROW_LIMIT: usize = 80;

/// Pan / zoom constants — same shape as `routes::graph` so the two
/// canvases feel identical under the hand. Tweaks here should mirror
/// there.
const MIN_ZOOM: f32 = 0.5;
const MAX_ZOOM: f32 = 4.0;
/// Sensitivity in `zoom_factor = exp(SCROLL_K * dy_px)`. 0.005 means
/// a 100-pixel scroll roughly halves or doubles the zoom.
const SCROLL_K: f32 = 0.005;
/// Press-vs-drag pixel threshold (Manhattan). Below this, mouse-up
/// falls through to click-selection; above, the gesture is committed
/// to a pan and the click is suppressed.
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

pub struct KnowledgeGraphRoute {
    state: Entity<AppState>,
    /// Tracks whether we've fired the initial fetch yet. The route
    /// re-renders on every `AppState` notification, but the fetch
    /// itself is one-shot per route lifetime — refreshes go through
    /// the manual button. Using a flag instead of comparing
    /// `state.memory_graph.is_some()` avoids re-fetching when an
    /// empty result is the genuine response (no drawers).
    fetched_once: bool,
    /// Selected node id (for the right-rail detail panel + canvas
    /// ring). Cleared by clicking the active row / pill again or the
    /// panel's × button.
    selected: Option<SharedString>,
    /// Cached force-directed layout for the canvas. Recomputed when
    /// the memory_graph response identity changes (cheap fingerprint
    /// of node + edge counts). The layout's `positions` and
    /// `edge_indices` index into `node_ids` below.
    layout: Option<Layout>,
    /// Fingerprint of the memory_graph the cached layout was built
    /// for. `(nodes_len, edges_len)` xor-folded — same trick as
    /// `routes::graph::GraphView`.
    layout_fingerprint: usize,
    /// Parallel array to `layout.positions` — node id at each layout
    /// index. Used by the canvas hit-test to map (x, y) clicks back
    /// to the SharedString id stored in `selected`.
    node_ids: Vec<SharedString>,
    /// Latest canvas bounds, written by `paint` and read by the click
    /// handler. Both fire on the gpui main thread sequentially — the
    /// mutex is just type-system glue across the two closures.
    last_canvas_bounds: Arc<Mutex<Option<Bounds<Pixels>>>>,
    /// Linear zoom factor around the canvas centre. 1.0 is the
    /// identity transform; higher spreads pills out, lower packs
    /// them in. Pill visual size is fixed in pixels so only positions
    /// transform — same convention as `routes::graph`.
    zoom: f32,
    /// Pan offset in unit (post-zoom) coords. Added to the visible
    /// position after the zoom transform; (0, 0) is the identity.
    pan: (f32, f32),
    /// `Some(_)` while the left button is held inside the canvas;
    /// `None` otherwise.
    drag: Option<DragState>,
}

impl KnowledgeGraphRoute {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self {
            state,
            fetched_once: false,
            selected: None,
            layout: None,
            layout_fingerprint: 0,
            node_ids: Vec::new(),
            last_canvas_bounds: Arc::new(Mutex::new(None)),
            zoom: 1.0,
            pan: (0.0, 0.0),
            drag: None,
        }
    }

    fn adjust_zoom(&mut self, dy_px: f32) {
        self.zoom = next_zoom(self.zoom, dy_px);
    }

    fn reset_view(&mut self) {
        self.zoom = 1.0;
        self.pan = (0.0, 0.0);
    }

    fn at_identity(&self) -> bool {
        is_identity_view(self.zoom, self.pan)
    }

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
        let bounds = match self.last_canvas_bounds.lock().ok().and_then(|g| *g) {
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
        if !drag.moved && dx_px.abs() + dy_px.abs() < DRAG_THRESHOLD_PX {
            return false;
        }
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

    fn ensure_fetch(&mut self, cx: &mut Context<Self>) {
        if !self.fetched_once {
            self.fetched_once = true;
            self.state.read(cx).submit_memory_graph();
        }
    }

    fn refresh(&self, cx: &mut Context<Self>) {
        self.state.read(cx).submit_memory_graph();
    }

    fn select(&mut self, id: SharedString) {
        if self.selected.as_deref() == Some(id.as_ref()) {
            self.selected = None;
        } else {
            self.selected = Some(id);
        }
    }

    /// (Re)compute the force-directed layout from the current memory
    /// graph if the response identity has changed. Caches both the
    /// `Layout` and a parallel `node_ids` vec so the canvas hit-test
    /// can map a click back to a `SharedString` id without re-walking
    /// the original snapshot.
    fn ensure_layout(&mut self, nodes: &[MemoryGraphNode], edges: &[MemoryGraphEdge]) {
        let fp = nodes.len() ^ (edges.len() << 16);
        if self.layout.is_some() && self.layout_fingerprint == fp {
            return;
        }
        let snapshot = GraphSnapshot {
            nodes: nodes
                .iter()
                .map(|n| GraphNode {
                    id: n.id.to_string(),
                    depth: 0,
                    kind: Some(n.kind.to_string()),
                    file: None,
                    line: None,
                    signature: None,
                })
                .collect(),
            edges: edges
                .iter()
                .map(|e| GraphEdge {
                    src: e.src.to_string(),
                    dst: e.dst.to_string(),
                })
                .collect(),
            seeds: Vec::new(),
        };
        self.layout = Some(graph_layout::run(&snapshot));
        self.layout_fingerprint = fp;
        self.node_ids = nodes.iter().map(|n| n.id.clone()).collect();
        // Layout shape changed — drop a stale selection so the
        // canvas ring doesn't point at the wrong pill.
        self.selected = None;
    }

    /// Convert a window-relative click position into the layout
    /// index of the nearest pill (within the hit tolerance), inverting
    /// the zoom + pan transform along the way. `None` if the click
    /// missed every pill.
    fn hit_test(&self, win_pos: gpui::Point<Pixels>) -> Option<usize> {
        let bounds = self.last_canvas_bounds.lock().ok()?.as_ref().copied()?;
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
        // Hit half-extents are fixed in pixels (pill visual size doesn't
        // scale with zoom), so in unit space they shrink by 1/zoom on
        // each axis — divide by `zoom` along with `w` / `h`.
        let half_w_u = (PILL_WIDTH * 0.5 + HIT_PADDING_PX) / (w * self.zoom);
        let half_h_u = (PILL_HEIGHT * 0.5 + HIT_PADDING_PX) / (h * self.zoom);
        let mut best: Option<(usize, f32)> = None;
        for (i, &(ux, uy)) in layout.positions.iter().enumerate() {
            let dx = (ux - u_x).abs();
            let dy = (uy - u_y).abs();
            if dx <= half_w_u && dy <= half_h_u {
                // Tie-break by Manhattan distance — closer click wins.
                let nd = dx + dy;
                if best.map(|(_, b)| nd < b).unwrap_or(true) {
                    best = Some((i, nd));
                }
            }
        }
        best.map(|(i, _)| i)
    }

    fn handle_canvas_click(&mut self, win_pos: gpui::Point<Pixels>) {
        match self.hit_test(win_pos) {
            Some(idx) => {
                if let Some(id) = self.node_ids.get(idx).cloned() {
                    self.select(id);
                }
            }
            None => self.selected = None,
        }
    }
}

impl Render for KnowledgeGraphRoute {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_fetch(cx);

        // Cross-route nav handoff (e.g. Knowledge → K-Graph): a prior
        // render staged a drawer id to pre-select. Apply once and let
        // the staged slot stay empty so a manual deselect doesn't get
        // re-overridden on the next notify tick.
        let pending_select = self
            .state
            .update(cx, |s, _| s.take_pending_kgraph_selected_id());
        if let Some(id) = pending_select {
            self.selected = Some(id);
        }

        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let foreground = theme.foreground;
        let border = theme.border;
        let secondary = theme.secondary;
        let primary = theme.primary;
        let danger = theme.danger;

        let state = self.state.read(cx);
        let graph = state.memory_graph.clone();
        let graph_error = state.memory_graph_error.clone();

        // ── Header + counters ─────────────────────────────────────
        let stats_label = match (graph.as_ref(), graph_error.as_ref()) {
            (Some(g), _) => format!(
                "· {} drawers · {} cross-refs",
                g.stats.drawers, g.stats.edges
            ),
            (None, Some(_)) => "· fetch failed (see below)".into(),
            (None, None) => "· loading…".into(),
        };
        let view_for_refresh = cx.entity();
        let refresh_btn = div()
            .id("k-graph-refresh")
            .px_2()
            .py_0p5()
            .border_1()
            .border_color(border)
            .rounded_md()
            .text_color(primary)
            .cursor_pointer()
            .hover(move |s| s.border_color(primary))
            .tooltip(|window, cx| Tooltip::new("Re-fetch the memory graph").build(window, cx))
            .child(SharedString::new_static("Refresh"))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                view_for_refresh.update(cx, |this, cx| {
                    this.refresh(cx);
                    cx.notify();
                });
            });

        let header = h_flex()
            .gap_3()
            .px_5()
            .py_3()
            .border_b_1()
            .border_color(border)
            .child(
                div()
                    .text_lg()
                    .text_color(foreground)
                    .child(SharedString::new_static("Knowledge graph")),
            )
            .child(
                div()
                    .text_color(muted)
                    .child(SharedString::from(stats_label)),
            )
            .child(div().flex_1())
            .child(refresh_btn);

        // ── Error pill (only when fetch failed) ───────────────────
        let error_block: gpui::AnyElement = match graph_error.as_deref() {
            Some(msg) => div()
                .mx_5()
                .mt_3()
                .px_3()
                .py_2()
                .border_1()
                .border_color(danger)
                .rounded_md()
                .bg(secondary)
                .text_color(danger)
                .text_xs()
                .child(SharedString::from(msg.to_string()))
                .into_any_element(),
            None => div().into_any_element(),
        };

        // ── Body ─────────────────────────────────────────────────
        // No graph yet → loading hint. Empty graph → empty hint.
        // Otherwise → wing-grouped node list + top-N edges section
        // + selected-node detail panel.
        let body: gpui::AnyElement = match graph {
            None => div()
                .mx_5()
                .mt_4()
                .text_color(muted)
                .child(SharedString::new_static("Loading memory graph…"))
                .into_any_element(),
            Some(g) if g.nodes.is_empty() => empty_state(
                "\u{25CC}",
                "No drawers in the memory backend yet",
                "Run `crabcc memory ingest` from the CLI to populate the graph.",
                muted,
                foreground,
            )
            .into_any_element(),
            Some(g) => {
                // Build / refresh the cached layout BEFORE the
                // closures capture the route entity; the layout
                // mutation is route-state, the canvas only reads.
                self.ensure_layout(&g.nodes, &g.edges);

                let edges_for_node = build_edge_index(&g.edges);
                let by_wing = group_by_wing(&g.nodes);

                // ── Canvas (top) ──────────────────────────────────
                let canvas_block = render_canvas(
                    self.layout.as_ref(),
                    &self.node_ids,
                    &g.nodes,
                    self.selected.as_ref(),
                    self.zoom,
                    self.pan,
                    self.at_identity(),
                    self.last_canvas_bounds.clone(),
                    cx.entity(),
                    foreground,
                    muted,
                    border,
                    secondary,
                    primary,
                    theme,
                );

                let view_for_select = cx.entity();
                let mut sections = v_flex().flex_1().gap_3().px_5().py_3().child(
                    div()
                        .text_color(muted)
                        .text_xs()
                        .child(SharedString::new_static("DRAWERS BY WING")),
                );
                for (wing, drawers) in by_wing {
                    let wing_color = wing_color(&wing, theme);
                    sections = sections.child(wing_section(
                        wing,
                        wing_color,
                        drawers,
                        &edges_for_node,
                        &self.selected,
                        muted,
                        foreground,
                        border,
                        secondary,
                        view_for_select.clone(),
                        self.state.clone(),
                    ));
                }

                // Right rail: selected drawer detail.
                let detail_panel = render_detail(
                    self.selected.as_ref(),
                    &g.nodes,
                    &g.edges,
                    self.state.clone(),
                    foreground,
                    muted,
                    border,
                    secondary,
                    primary,
                );

                v_flex()
                    .size_full()
                    .child(canvas_block)
                    .child(h_flex().size_full().child(sections).child(detail_panel))
                    .into_any_element()
            }
        };

        v_flex()
            .size_full()
            .child(header)
            .child(error_block)
            .child(body)
    }
}

pub(crate) mod paint;
use paint::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_layout::Layout;

    fn mk_node(id: &str, title: &str) -> MemoryGraphNode {
        MemoryGraphNode {
            id: SharedString::from(id.to_string()),
            title: SharedString::from(title.to_string()),
            kind: SharedString::from("project".to_string()),
            ts: 0,
            len: 0,
        }
    }

    #[test]
    fn pick_hub_labels_filters_by_min_degree() {
        // 4 nodes: 0 connected to 1,2,3 (degree 3) and 1↔2 (each
        // earns one more, ending at 2). Only node 0 should label.
        let nodes = vec![
            mk_node("a", "Alpha"),
            mk_node("b", "Bravo"),
            mk_node("c", "Charlie"),
            mk_node("d", "Delta"),
        ];
        let layout = Layout {
            positions: vec![(0.5, 0.5), (0.1, 0.1), (0.9, 0.1), (0.5, 0.9)],
            edge_indices: vec![(0, 1), (0, 2), (0, 3), (1, 2)],
        };
        let hubs = pick_hub_labels(&layout, &nodes);
        assert_eq!(hubs.len(), 1);
        assert_eq!(hubs[0].index, 0);
        assert_eq!(hubs[0].title.as_ref(), "Alpha");
    }

    #[test]
    fn pick_hub_labels_caps_at_max() {
        // Build a star with 12 leaves (degree-1) around 1 hub
        // (degree 12) — only the hub qualifies, but also build 9
        // mutually-connected nodes so we exceed `MAX_HUB_LABELS`.
        let mut nodes: Vec<MemoryGraphNode> = (0..12)
            .map(|i| mk_node(&format!("n{i}"), &format!("Node {i}")))
            .collect();
        // Push 9 hub nodes, each connected to 3 distinct leaves.
        nodes.extend((12..21).map(|i| mk_node(&format!("h{i}"), &format!("Hub {i}"))));
        let positions = (0..nodes.len())
            .map(|i| (i as f32 / nodes.len() as f32, 0.5))
            .collect();
        let mut edge_indices: Vec<(usize, usize)> = Vec::new();
        for hub in 12..21 {
            for leaf in 0..3 {
                edge_indices.push((hub, leaf));
            }
        }
        let layout = Layout {
            positions,
            edge_indices,
        };
        let hubs = pick_hub_labels(&layout, &nodes);
        assert_eq!(hubs.len(), MAX_HUB_LABELS);
    }

    #[test]
    fn truncate_label_passes_short_strings_through() {
        assert_eq!(truncate_label("Short title"), "Short title");
    }

    #[test]
    fn truncate_label_ellipsizes_long_strings() {
        let long = "a".repeat(60);
        let trimmed = truncate_label(&long);
        let expected_chars = HUB_LABEL_MAX_CHARS;
        assert_eq!(trimmed.chars().count(), expected_chars);
        assert!(trimmed.ends_with('…'));
    }

    #[test]
    fn is_identity_view_passes_at_origin() {
        assert!(is_identity_view(1.0, (0.0, 0.0)));
    }

    #[test]
    fn is_identity_view_fails_for_non_identity() {
        assert!(!is_identity_view(2.0, (0.0, 0.0)));
        assert!(!is_identity_view(1.0, (0.1, 0.0)));
        assert!(!is_identity_view(1.0, (0.0, -0.1)));
    }

    #[test]
    fn next_zoom_clamps_to_max() {
        // A massive positive dy must not exceed MAX_ZOOM.
        let z = next_zoom(1.0, 100_000.0);
        assert!((z - MAX_ZOOM).abs() < f32::EPSILON);
    }

    #[test]
    fn next_zoom_clamps_to_min() {
        let z = next_zoom(1.0, -100_000.0);
        assert!((z - MIN_ZOOM).abs() < f32::EPSILON);
    }

    #[test]
    fn next_zoom_zero_dy_is_noop() {
        let z = next_zoom(1.7, 0.0);
        assert!((z - 1.7).abs() < 1e-6);
    }

    #[test]
    fn neighbours_of_returns_empty_when_no_selection() {
        let edges = vec![(0, 1), (1, 2)];
        assert!(neighbours_of(None, &edges).is_empty());
    }

    #[test]
    fn neighbours_of_finds_both_endpoints() {
        // Star: 0 connected to 1, 2, 3. Self-loops and reverse
        // direction both count.
        let edges = vec![(0, 1), (2, 0), (3, 0), (1, 2)];
        let n = neighbours_of(Some(0), &edges);
        assert_eq!(n.len(), 3);
        assert!(n.contains(&1));
        assert!(n.contains(&2));
        assert!(n.contains(&3));
    }

    #[test]
    fn neighbours_of_excludes_disconnected_nodes() {
        let edges = vec![(0, 1), (2, 3)];
        let n = neighbours_of(Some(0), &edges);
        assert_eq!(n.len(), 1);
        assert!(n.contains(&1));
        assert!(!n.contains(&2));
        assert!(!n.contains(&3));
    }

    #[test]
    fn truncate_label_respects_codepoints() {
        // Multibyte characters — must not slice mid-codepoint.
        let s = "α".repeat(60);
        let trimmed = truncate_label(&s);
        assert_eq!(trimmed.chars().count(), HUB_LABEL_MAX_CHARS);
        assert!(trimmed.ends_with('…'));
    }
}
