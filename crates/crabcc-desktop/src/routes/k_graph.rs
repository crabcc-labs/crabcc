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
//!     node to select; click empty canvas to deselect.
//!   * the **wing-grouped list** below the canvas — same data, easier
//!     to scan at scale.
//!   * a **right-rail Drawer Detail** panel on the active selection
//!     (incoming + outgoing edge lists, `via` annotation).
//!
//! What's deliberately not here yet:
//!
//!   * Pan / zoom on the canvas. The relations graph has it; the
//!     memory graph rarely tops a few hundred drawers, so a static
//!     fit-to-bounds layout is enough for v1. Promote when the
//!     deque outgrows the visible canvas at typical density.
//!   * Dashed edges. gpui's `PathBuilder` doesn't expose a dash
//!     pattern; faking it via short segments is a polish item.
//!     Edge muting (alpha 0.45) is enough differentiation today.
//!
//! State is stored on `AppState::memory_graph` (lazy fetch on first
//! render via `submit_memory_graph`; manual refresh button re-runs
//! the same path). Errors land on `AppState::memory_graph_error` and
//! render inline.

use std::collections::HashSet;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use gpui::{
    canvas, div, point, prelude::*, px, Bounds, Context, Entity, Hsla, IntoElement, MouseButton,
    PathBuilder, Pixels, Render, SharedString, Window,
};
use gpui_component::{
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex, ActiveTheme,
};

use crate::api::types::{GraphEdge, GraphNode, GraphSnapshot, MemoryGraphEdge, MemoryGraphNode};
use crate::graph_layout::{self, Layout};
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
    /// gpui-component InputState — owns the filter text + focus
    /// handle. Substring matched against drawer title + id.
    query_input: Entity<InputState>,
    /// Lower-cased mirror of the input's value, kept in sync via
    /// `InputEvent::Change`. Avoids re-lowercasing on every match
    /// check during render.
    query_lower: String,
    /// Active wing pill set. Empty = all wings allowed (default);
    /// non-empty = only nodes whose wing is in the set survive
    /// the filter. Multi-select via click on a wing pill.
    wing_filter: HashSet<SharedString>,
}

impl KnowledgeGraphRoute {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        let query_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("filter by title or id…"));
        cx.subscribe_in(&query_input, window, |this, st, event, _, cx| {
            if let InputEvent::Change = event {
                this.query_lower = st.read(cx).value().to_string().to_lowercase();
                // Force a layout recompute since the visible node
                // set just changed shape.
                this.layout = None;
                cx.notify();
            }
        })
        .detach();
        Self {
            state,
            fetched_once: false,
            selected: None,
            layout: None,
            layout_fingerprint: 0,
            node_ids: Vec::new(),
            last_canvas_bounds: Arc::new(Mutex::new(None)),
            query_input,
            query_lower: String::new(),
            wing_filter: HashSet::new(),
        }
    }

    /// Predicate: does `n` survive the current filter set? Wing
    /// filter is a positive whitelist (empty = allow all);
    /// query_lower must appear as a substring of the lower-cased
    /// title or id when set.
    fn node_matches(&self, n: &MemoryGraphNode) -> bool {
        if !self.wing_filter.is_empty() && !self.wing_filter.contains(&n.kind) {
            return false;
        }
        if self.query_lower.is_empty() {
            return true;
        }
        let q = self.query_lower.as_str();
        n.title.to_lowercase().contains(q) || n.id.to_lowercase().contains(q)
    }

    fn toggle_wing(&mut self, wing: SharedString) {
        if !self.wing_filter.remove(&wing) {
            self.wing_filter.insert(wing);
        }
        // Layout shape changed — drop the cached layout so the next
        // render rebuilds from the filtered subset.
        self.layout = None;
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
    /// index of the nearest pill (within the hit tolerance). `None`
    /// if the click missed every pill.
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
        // Pills are axis-aligned rectangles; a hit is any (x, y)
        // inside the bounding box plus the tolerance in each axis.
        let half_w = PILL_WIDTH * 0.5 + HIT_PADDING_PX;
        let half_h = PILL_HEIGHT * 0.5 + HIT_PADDING_PX;
        let mut best: Option<(usize, f32)> = None;
        for (i, &(ux, uy)) in layout.positions.iter().enumerate() {
            let cx = ux * w;
            let cy = uy * h;
            let dx = (local_x - cx).abs();
            let dy = (local_y - cy).abs();
            if dx <= half_w && dy <= half_h {
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
            Some(g) if g.nodes.is_empty() => div()
                .mx_5()
                .mt_4()
                .text_color(muted)
                .child(SharedString::new_static(
                    "No drawers in the memory backend yet — run `crabcc memory ingest` to populate.",
                ))
                .into_any_element(),
            Some(g) => {
                // Apply the filter pre-pass: prune nodes that don't
                // match query + wing filter, then drop edges whose
                // endpoints fell out. The canvas + list + edge index
                // all operate on the filtered subset, which keeps
                // the visual experience consistent — a hidden node
                // never appears in any tail.
                let filtered_nodes: Vec<MemoryGraphNode> =
                    g.nodes.iter().filter(|n| self.node_matches(n)).cloned().collect();
                let kept_ids: HashSet<&SharedString> =
                    filtered_nodes.iter().map(|n| &n.id).collect();
                let filtered_edges: Vec<MemoryGraphEdge> = g
                    .edges
                    .iter()
                    .filter(|e| kept_ids.contains(&e.src) && kept_ids.contains(&e.dst))
                    .cloned()
                    .collect();

                // Build / refresh the cached layout from the FILTERED
                // subset. Filter changes invalidate the cache (`layout
                // = None` in toggle_wing / on InputEvent::Change).
                self.ensure_layout(&filtered_nodes, &filtered_edges);

                let edges_for_node = build_edge_index(&filtered_edges);
                let by_wing = group_by_wing(&filtered_nodes);

                // ── Filter strip (above canvas) ───────────────────
                // Wing pills + substring input. Both narrow the
                // canvas + list together via `node_matches`.
                let filter_strip = render_filter_strip(
                    &g.nodes,
                    &self.wing_filter,
                    &self.query_input,
                    filtered_nodes.len(),
                    g.nodes.len(),
                    cx.entity(),
                    muted,
                    foreground,
                    border,
                    secondary,
                    theme,
                );

                // ── Canvas (top) ──────────────────────────────────
                let canvas_block = render_canvas(
                    self.layout.as_ref(),
                    &self.node_ids,
                    &filtered_nodes,
                    self.selected.as_ref(),
                    self.last_canvas_bounds.clone(),
                    cx.entity(),
                    foreground,
                    muted,
                    border,
                    secondary,
                    theme,
                );

                let view_for_select = cx.entity();
                let mut sections = v_flex()
                    .flex_1()
                    .gap_3()
                    .px_5()
                    .py_3()
                    .child(
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
                    ));
                }

                // Right rail: selected drawer detail. Always uses
                // the FULL graph (not the filtered subset) so a node
                // selected before the filter narrowed still resolves
                // — otherwise stale-selection state would dead-end.
                let detail_panel = render_detail(
                    self.selected.as_ref(),
                    &g.nodes,
                    &g.edges,
                    foreground,
                    muted,
                    border,
                    secondary,
                    primary,
                );

                v_flex()
                    .size_full()
                    .child(filter_strip)
                    .child(canvas_block)
                    .child(
                        h_flex()
                            .size_full()
                            .child(sections)
                            .child(detail_panel),
                    )
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

/// Sum (incoming, outgoing) edges per drawer id. Used by the row
/// rendering to label each drawer with its "→ N · ← M" tail.
type EdgeIndex = HashMap<SharedString, (usize, usize)>;

fn build_edge_index(edges: &[MemoryGraphEdge]) -> EdgeIndex {
    let mut idx: EdgeIndex = HashMap::new();
    for e in edges {
        idx.entry(e.src.clone()).or_default().1 += 1;
        idx.entry(e.dst.clone()).or_default().0 += 1;
    }
    idx
}

fn group_by_wing(nodes: &[MemoryGraphNode]) -> Vec<(SharedString, Vec<MemoryGraphNode>)> {
    // BTreeMap so the wing order is deterministic across renders.
    let mut by: BTreeMap<SharedString, Vec<MemoryGraphNode>> = BTreeMap::new();
    for n in nodes {
        by.entry(n.kind.clone()).or_default().push(n.clone());
    }
    // Inside each wing: newest first. ts is unix-seconds.
    let mut out: Vec<(SharedString, Vec<MemoryGraphNode>)> = by.into_iter().collect();
    for (_, drawers) in out.iter_mut() {
        drawers.sort_by_key(|n| std::cmp::Reverse(n.ts));
    }
    out
}

/// Map wing name → tone. Mirrors the brief's wing pill palette
/// (agents=primary, feedback=info, project=success, reference=warning,
/// user=danger). Unknown wings fall back to `muted`.
fn wing_color(wing: &str, theme: &gpui_component::Theme) -> Hsla {
    match wing {
        "agents" => theme.primary,
        "feedback" => theme.info,
        "project" => theme.success,
        "reference" => theme.warning,
        "user" => theme.danger,
        _ => theme.muted_foreground,
    }
}

#[allow(clippy::too_many_arguments)]
fn wing_section(
    wing: SharedString,
    wing_col: Hsla,
    drawers: Vec<MemoryGraphNode>,
    edges: &EdgeIndex,
    selected: &Option<SharedString>,
    muted: Hsla,
    foreground: Hsla,
    border: Hsla,
    secondary: Hsla,
    view: Entity<KnowledgeGraphRoute>,
) -> gpui::Div {
    let total = drawers.len();
    let visible = drawers.len().min(SECTION_ROW_LIMIT);
    let header = h_flex()
        .gap_2()
        .child(
            div()
                .px_2()
                .py_0p5()
                .border_1()
                .border_color(wing_col)
                .rounded_md()
                .text_color(wing_col)
                .text_xs()
                .child(SharedString::from(wing.to_string())),
        )
        .child(
            div()
                .text_color(muted)
                .text_xs()
                .child(SharedString::from(format!(
                    "{visible} of {total} drawer{}",
                    if total == 1 { "" } else { "s" }
                ))),
        );
    let mut rows = v_flex().gap_0p5();
    for d in drawers.into_iter().take(SECTION_ROW_LIMIT) {
        let id = d.id.clone();
        let is_selected = selected.as_deref() == Some(id.as_ref());
        let (incoming, outgoing) = edges.get(&id).copied().unwrap_or((0, 0));
        let row_view = view.clone();
        let id_for_click = id.clone();
        rows = rows.child(
            h_flex()
                .id(SharedString::from(format!(
                    "k-graph-row-{}",
                    sanitize_id_part(&id)
                )))
                .gap_3()
                .px_2()
                .py_0p5()
                .border_1()
                .border_color(if is_selected {
                    wing_col
                } else {
                    gpui::transparent_black()
                })
                .rounded_md()
                .bg(if is_selected {
                    secondary
                } else {
                    gpui::transparent_black()
                })
                .child(
                    div()
                        .min_w(px(220.0))
                        .text_color(foreground)
                        .text_xs()
                        .child(SharedString::from(d.title.to_string())),
                )
                .child(
                    div()
                        .text_color(muted)
                        .text_xs()
                        .child(SharedString::from(format!("← {incoming} · → {outgoing}"))),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .text_color(muted)
                        .text_xs()
                        .child(SharedString::from(format!("{} B", d.len))),
                )
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    let target = id_for_click.clone();
                    row_view.update(cx, |this, cx| {
                        this.select(target);
                        cx.notify();
                    });
                }),
        );
    }
    if total > SECTION_ROW_LIMIT {
        rows = rows.child(
            div()
                .pl_2()
                .text_color(muted)
                .text_xs()
                .child(SharedString::from(format!(
                    "+ {} more (cap at {SECTION_ROW_LIMIT})",
                    total - SECTION_ROW_LIMIT
                ))),
        );
    }
    let _ = border;
    v_flex().gap_2().child(header).child(rows)
}

/// Replace anything that's not `[A-Za-z0-9_-]` with `_` so the
/// resulting id is gpui::ElementId-safe (it doesn't enforce this,
/// but unstable element ids cause re-mount churn between renders).
fn sanitize_id_part(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn render_detail(
    selected: Option<&SharedString>,
    nodes: &[MemoryGraphNode],
    edges: &[MemoryGraphEdge],
    foreground: Hsla,
    muted: Hsla,
    border: Hsla,
    secondary: Hsla,
    primary: Hsla,
) -> gpui::Div {
    let frame = v_flex()
        .w(px(340.0))
        .gap_3()
        .p_3()
        .border_l_1()
        .border_color(border)
        .bg(secondary);
    let Some(id) = selected else {
        return frame.child(div().text_color(muted).child(SharedString::new_static(
            "Click a drawer to inspect its cross-refs.",
        )));
    };
    let Some(node) = nodes.iter().find(|n| n.id == *id) else {
        return frame.child(
            div()
                .text_color(muted)
                .child(SharedString::new_static("(node not found)")),
        );
    };
    let mut incoming: Vec<&MemoryGraphEdge> = edges.iter().filter(|e| e.dst == *id).collect();
    let mut outgoing: Vec<&MemoryGraphEdge> = edges.iter().filter(|e| e.src == *id).collect();
    // Stable sort by counterpart id so the panel doesn't shuffle on
    // every render against a noisy edge list.
    incoming.sort_by(|a, b| a.src.as_ref().cmp(b.src.as_ref()));
    outgoing.sort_by(|a, b| a.dst.as_ref().cmp(b.dst.as_ref()));

    let header = v_flex()
        .gap_0p5()
        .child(
            div()
                .text_color(foreground)
                .child(SharedString::from(node.title.to_string())),
        )
        .child(
            div()
                .text_color(muted)
                .text_xs()
                .child(SharedString::from(format!(
                    "wing {} · {} bytes · {}",
                    node.kind, node.len, node.id
                ))),
        );

    let in_block = v_flex()
        .gap_0p5()
        .child(
            div()
                .text_color(muted)
                .text_xs()
                .child(SharedString::from(format!("INCOMING ({})", incoming.len()))),
        )
        .children(incoming.into_iter().map(|e| {
            div()
                .pl_2()
                .text_color(foreground)
                .text_xs()
                .child(SharedString::from(format!("{} (via {})", e.src, e.via)))
                .into_any_element()
        }));

    let out_block = v_flex()
        .gap_0p5()
        .child(
            div()
                .text_color(muted)
                .text_xs()
                .child(SharedString::from(format!("OUTGOING ({})", outgoing.len()))),
        )
        .children(outgoing.into_iter().map(|e| {
            div()
                .pl_2()
                .text_color(foreground)
                .text_xs()
                .child(SharedString::from(format!("{} (via {})", e.dst, e.via)))
                .into_any_element()
        }));

    let _ = primary;
    frame.child(header).child(in_block).child(out_block)
}

/// Build the canvas block — header strip + click-handling
/// `gpui::canvas` element of fixed [`CANVAS_HEIGHT`]. Returns an
/// empty div if the layout isn't ready (no drawers yet).
#[allow(clippy::too_many_arguments)]
fn render_canvas(
    layout: Option<&Layout>,
    node_ids: &[SharedString],
    nodes: &[MemoryGraphNode],
    selected: Option<&SharedString>,
    bounds_share: Arc<Mutex<Option<Bounds<Pixels>>>>,
    view: Entity<KnowledgeGraphRoute>,
    foreground: Hsla,
    muted: Hsla,
    border: Hsla,
    secondary: Hsla,
    theme: &gpui_component::Theme,
) -> gpui::AnyElement {
    let Some(layout) = layout else {
        return div().into_any_element();
    };
    if layout.positions.is_empty() {
        return div().into_any_element();
    }

    // Pre-resolve per-node tones — pill colour follows wing, since
    // the K-Graph palette differs from the relations graph's primary
    // monochrome. The closures don't borrow `theme`, so this resolves
    // before paint and is cheap (each node = one match).
    let node_tones: Vec<Hsla> = nodes.iter().map(|n| wing_color(&n.kind, theme)).collect();

    // Map the selected id to a layout index so paint can ring the
    // correct pill without re-walking node_ids on every frame.
    let selected_idx: Option<usize> =
        selected.and_then(|sel| node_ids.iter().position(|id| id == sel));

    let layout_clone = layout.clone();
    let node_tones_clone = node_tones.clone();

    let canvas_el = canvas(
        move |_bounds, _, _| (),
        move |bounds: Bounds<Pixels>, _, window, _| {
            if let Ok(mut guard) = bounds_share.lock() {
                *guard = Some(bounds);
            }
            paint_k_graph(
                bounds,
                &layout_clone,
                &node_tones_clone,
                selected_idx,
                foreground,
                muted,
                window,
            );
        },
    )
    .size_full();

    let entity_for_click = view.clone();
    let canvas_container = div()
        .id("k-graph-canvas")
        .size_full()
        .child(canvas_el)
        .on_mouse_down(MouseButton::Left, move |event, _, cx| {
            let pos = event.position;
            entity_for_click.update(cx, |this, cx| {
                this.handle_canvas_click(pos);
                cx.notify();
            });
        });

    v_flex()
        .mx_5()
        .mt_3()
        .px_3()
        .py_2()
        .border_1()
        .border_color(border)
        .rounded_md()
        .bg(secondary)
        .gap_2()
        .child(
            h_flex()
                .gap_2()
                .child(
                    div()
                        .text_color(muted)
                        .text_xs()
                        .child(SharedString::new_static("CANVAS")),
                )
                .child(
                    div()
                        .text_color(muted)
                        .text_xs()
                        .child(SharedString::from(format!(
                            "{} pills · {} cross-refs",
                            layout.positions.len(),
                            layout.edge_indices.len()
                        ))),
                ),
        )
        .child(div().h(px(CANVAS_HEIGHT)).child(canvas_container))
        .into_any_element()
}

/// Dash period for memory-canvas edges in pixels. `DASH_ON_PX`
/// segment painted, `DASH_OFF_PX` skipped, repeat. gpui's
/// `PathBuilder` doesn't expose a dash pattern, so we walk each edge
/// in fixed pixel steps and paint every other segment manually.
/// 5/4 reads as a clear dash pattern at 1px stroke width without
/// fragmenting too much on short edges.
const DASH_ON_PX: f32 = 5.0;
const DASH_OFF_PX: f32 = 4.0;

/// Paint pass for the memory canvas. Differentiated from the
/// relations graph in three ways:
///
///   * Nodes are wide rounded-rect "pills" (`PILL_WIDTH × PILL_HEIGHT`)
///     instead of small circles.
///   * Edges are **dashed** thin lines (manual segment painting —
///     gpui's `PathBuilder` doesn't expose a dash pattern; we walk
///     each edge in `DASH_ON_PX` / `DASH_OFF_PX` steps and paint
///     every other segment). At low alpha so the eye reads pills
///     first.
///   * Selection ring uses `foreground` (off-white) instead of the
///     relations graph's `primary` purple — same idea (highlighted
///     pill stands out) but a deliberately different hue so a user
///     bouncing between routes never confuses the two graphs.
fn paint_k_graph(
    bounds: Bounds<Pixels>,
    layout: &Layout,
    node_tones: &[Hsla],
    selected: Option<usize>,
    foreground: Hsla,
    muted: Hsla,
    window: &mut Window,
) {
    let ox = f32::from(bounds.origin.x);
    let oy = f32::from(bounds.origin.y);
    let w = f32::from(bounds.size.width);
    let h = f32::from(bounds.size.height);

    let to_px = |(ux, uy): (f32, f32)| point(px(ox + ux * w), px(oy + uy * h));

    // Edges first so pills paint on top. Dashed via manual segment
    // walk — see `paint_dashed_edge`.
    let edge_color = with_alpha(muted, 0.45);
    for &(a, b) in &layout.edge_indices {
        if let (Some(p1), Some(p2)) = (layout.positions.get(a), layout.positions.get(b)) {
            // Convert unit-coord endpoints to canvas pixels here so
            // the dash walk operates in pixel space (consistent
            // dash period regardless of canvas size).
            let p1_px = to_px(*p1);
            let p2_px = to_px(*p2);
            paint_dashed_edge(p1_px, p2_px, edge_color, window);
        }
    }

    // Pills. Each gets its wing colour at base alpha; the selected
    // pill paints again with a 2-px foreground ring (drawn as a
    // slightly larger filled quad below the inner fill).
    let half_w = PILL_WIDTH * 0.5;
    let half_h = PILL_HEIGHT * 0.5;
    for (i, &(ux, uy)) in layout.positions.iter().enumerate() {
        let centre = point(px(ox + ux * w), px(oy + uy * h));
        let tone = node_tones.get(i).copied().unwrap_or(muted);
        if Some(i) == selected {
            // Foreground ring: a slightly larger quad in foreground
            // colour painted first, then the wing-coloured pill on
            // top — cheap "ring around pill" without a stroked path.
            let ring_w = PILL_WIDTH + 4.0;
            let ring_h = PILL_HEIGHT + 4.0;
            let ring_bounds = Bounds {
                origin: point(centre.x - px(ring_w * 0.5), centre.y - px(ring_h * 0.5)),
                size: gpui::size(px(ring_w), px(ring_h)),
            };
            window.paint_quad(
                gpui::fill(ring_bounds, foreground)
                    .corner_radii(gpui::Corners::all(px(ring_h * 0.5))),
            );
        }
        let pill_bounds = Bounds {
            origin: point(centre.x - px(half_w), centre.y - px(half_h)),
            size: gpui::size(px(PILL_WIDTH), px(PILL_HEIGHT)),
        };
        window
            .paint_quad(gpui::fill(pill_bounds, tone).corner_radii(gpui::Corners::all(px(half_h))));
    }
}

fn with_alpha(c: Hsla, a: f32) -> Hsla {
    Hsla { a, ..c }
}

/// Filter strip rendered above the canvas. Two pieces:
///
///   * Wing pills, derived from the unique `kind` values present
///     in the unfiltered response. Click toggles in/out of
///     `wing_filter`. Active pills get a filled bg in the wing's
///     own colour; inactive pills are outlined-only.
///   * Substring input matched against title + id (lowercase).
///
/// A counter line shows `N of M visible` so the user can tell at
/// a glance how aggressive the current filter is.
#[allow(clippy::too_many_arguments)]
fn render_filter_strip(
    nodes: &[MemoryGraphNode],
    wing_filter: &HashSet<SharedString>,
    query_input: &Entity<InputState>,
    visible_count: usize,
    total_count: usize,
    view: Entity<KnowledgeGraphRoute>,
    muted: Hsla,
    foreground: Hsla,
    border: Hsla,
    secondary: Hsla,
    theme: &gpui_component::Theme,
) -> gpui::Div {
    // Unique wings, sorted alphabetically so the pill order is
    // stable across renders. BTreeSet would also work; HashSet +
    // sort keeps the iteration explicit.
    let mut wings: Vec<SharedString> = nodes
        .iter()
        .map(|n| n.kind.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    wings.sort_by(|a, b| a.as_ref().cmp(b.as_ref()));

    let pill_iter = wings.into_iter().map(|wing| {
        let is_active = wing_filter.contains(&wing);
        let tone = wing_color(&wing, theme);
        let bg = if is_active {
            with_alpha(tone, 0.18)
        } else {
            gpui::transparent_black()
        };
        let id_str = format!("k-graph-wing-pill-{}", wing);
        let entity = view.clone();
        let wing_for_click = wing.clone();
        div()
            .id(SharedString::from(id_str))
            .px_2()
            .py_0p5()
            .border_1()
            .border_color(if is_active { tone } else { border })
            .rounded_md()
            .text_color(if is_active { foreground } else { muted })
            .bg(bg)
            .text_xs()
            .child(SharedString::from(wing.to_string()))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                let target = wing_for_click.clone();
                entity.update(cx, |this, cx| {
                    this.toggle_wing(target);
                    cx.notify();
                });
            })
            .into_any_element()
    });

    let pill_row = h_flex().gap_2().children(pill_iter);

    let search_field = div()
        .flex_1()
        .border_1()
        .border_color(border)
        .rounded_md()
        .px_2()
        .py_1()
        .child(Input::new(query_input).appearance(false));

    let counter = div()
        .text_color(muted)
        .text_xs()
        .child(SharedString::from(format!(
            "{visible_count} of {total_count} visible"
        )));

    v_flex()
        .mx_5()
        .mt_3()
        .gap_2()
        .px_3()
        .py_2()
        .border_1()
        .border_color(border)
        .rounded_md()
        .bg(secondary)
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(pill_row)
                .child(div().flex_1())
                .child(counter),
        )
        .child(search_field)
}

/// Walk the segment from `p1` to `p2` in `DASH_ON_PX + DASH_OFF_PX`
/// steps, painting only the "on" half each cycle. Cheap: O(length /
/// period) `paint_path` calls per edge — for a ~400px edge that's
/// ~45 short paths, well under per-frame budget for the bounded
/// edge counts the K-Graph deals with (a few hundred at most).
///
/// Short edges (length < `DASH_ON_PX`) paint as a single solid
/// segment — looks better than a single-pixel dash that would just
/// disappear at low alpha.
fn paint_dashed_edge(
    p1: gpui::Point<Pixels>,
    p2: gpui::Point<Pixels>,
    color: Hsla,
    window: &mut Window,
) {
    let x1 = f32::from(p1.x);
    let y1 = f32::from(p1.y);
    let x2 = f32::from(p2.x);
    let y2 = f32::from(p2.y);
    let dx = x2 - x1;
    let dy = y2 - y1;
    let len = (dx * dx + dy * dy).sqrt();
    if len <= f32::EPSILON {
        return;
    }
    if len <= DASH_ON_PX {
        // Edge is shorter than a single dash — paint solid.
        let mut pb = PathBuilder::stroke(px(EDGE_WIDTH));
        pb.move_to(p1);
        pb.line_to(p2);
        if let Ok(path) = pb.build() {
            window.paint_path(path, color);
        }
        return;
    }
    let nx = dx / len;
    let ny = dy / len;
    let period = DASH_ON_PX + DASH_OFF_PX;
    let mut t = 0.0_f32;
    while t < len {
        let t0 = t;
        let t1 = (t + DASH_ON_PX).min(len);
        let s0 = point(px(x1 + nx * t0), px(y1 + ny * t0));
        let s1 = point(px(x1 + nx * t1), px(y1 + ny * t1));
        let mut pb = PathBuilder::stroke(px(EDGE_WIDTH));
        pb.move_to(s0);
        pb.line_to(s1);
        if let Ok(path) = pb.build() {
            window.paint_path(path, color);
        }
        t += period;
    }
}
