//! Low-level render and layout helpers for the knowledge-graph route.
//! All functions here are called from the parent `Render` impl in `mod.rs`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use gpui::{
    canvas, div, point, prelude::*, px, App, Bounds, ClipboardItem, Context, Entity, Hsla,
    IntoElement, MouseButton, PathBuilder, Pixels, SharedString, TextRun, Window,
};
use gpui_component::{h_flex, tooltip::Tooltip, v_flex, ActiveTheme};

use crate::api::types::{GraphEdge, GraphNode, GraphSnapshot, MemoryGraphEdge, MemoryGraphNode};
use crate::graph_layout::{self, Layout};
use crate::routes::empty::empty_state;
use crate::state::AppState;

use super::{
    DragState, KnowledgeGraphRoute, CANVAS_HEIGHT, DRAG_THRESHOLD_PX, EDGE_WIDTH, HIT_PADDING_PX,
    MAX_ZOOM, MIN_ZOOM, PILL_HEIGHT, PILL_WIDTH, SCROLL_K, SECTION_ROW_LIMIT,
};

/// Sum (incoming, outgoing) edges per drawer id. Used by the row
/// rendering to label each drawer with its "→ N · ← M" tail.
pub(super) type EdgeIndex = HashMap<SharedString, (usize, usize)>;

pub(super) fn build_edge_index(edges: &[MemoryGraphEdge]) -> EdgeIndex {
    let mut idx: EdgeIndex = HashMap::new();
    for e in edges {
        idx.entry(e.src.clone()).or_default().1 += 1;
        idx.entry(e.dst.clone()).or_default().0 += 1;
    }
    idx
}

pub(super) fn group_by_wing(
    nodes: &[MemoryGraphNode],
) -> Vec<(SharedString, Vec<MemoryGraphNode>)> {
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
pub(super) fn wing_color(wing: &str, theme: &gpui_component::Theme) -> Hsla {
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
pub(super) fn wing_section(
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
    state_entity: Entity<AppState>,
) -> gpui::Div {
    let total = drawers.len();
    let visible = drawers.len().min(SECTION_ROW_LIMIT);
    // Wing badge — click navigates to Knowledge with that wing
    // pre-pinned, mirroring the established cross-route handoff
    // pattern. From the canvas-view's wing grouping the user can
    // jump straight to that wing's drawer body view.
    let badge_id: gpui::ElementId =
        SharedString::from(format!("k-graph-wing-link-{}", sanitize_id_part(&wing))).into();
    let nav_wing = wing.clone();
    let nav_state = state_entity.clone();
    let header = h_flex()
        .gap_2()
        .child(
            div()
                .id(badge_id)
                .px_2()
                .py_0p5()
                .border_1()
                .border_color(wing_col)
                .rounded_md()
                .text_color(wing_col)
                .text_xs()
                .cursor_pointer()
                .hover(move |s| s.bg(secondary))
                .child(SharedString::from(wing.to_string()))
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    let w = nav_wing.clone();
                    nav_state.update(cx, |s, cx| {
                        s.navigate_to_knowledge_with_wing_pin(w);
                        cx.notify();
                    });
                }),
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
        let row_tooltip: SharedString = if is_selected {
            SharedString::from(format!("Close drawer {} details", d.title))
        } else {
            SharedString::from(format!("Open drawer {}", d.title))
        };
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
                .cursor_pointer()
                .hover(move |s| s.border_color(wing_col))
                .tooltip(move |window, cx| Tooltip::new(row_tooltip.clone()).build(window, cx))
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
pub(super) fn sanitize_id_part(s: &str) -> String {
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
pub(super) fn render_detail(
    selected: Option<&SharedString>,
    nodes: &[MemoryGraphNode],
    edges: &[MemoryGraphEdge],
    state: Entity<AppState>,
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

    // "→ Knowledge" cross-link — pre-filters the Knowledge route to
    // this drawer's id. Drawer body lives there (this canvas only
    // shows wing + cross-refs), so this is the natural dive when the
    // user wants to read the actual content.
    let nav_id = node.id.clone();
    let nav_state = state.clone();
    let knowledge_link = div()
        .id("k-graph-detail-to-knowledge")
        .px_2()
        .py_0p5()
        .border_1()
        .border_color(border)
        .rounded_md()
        .text_color(primary)
        .text_xs()
        .cursor_pointer()
        .hover(move |s| s.border_color(primary))
        .tooltip(|window, cx| {
            Tooltip::new("Open Knowledge filtered to this drawer").build(window, cx)
        })
        .child(SharedString::new_static("\u{2192} Knowledge"))
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            let id = nav_id.clone();
            nav_state.update(cx, |s, cx| {
                s.navigate_to_knowledge_with_filter(id);
                cx.notify();
            });
        });

    // Subline: wing · bytes · source_id. The id is split into its
    // own click-to-copy chip — same pattern as the relations-graph
    // drawer's file:line (#479). Clipboard receives the raw id (no
    // surrounding "·" or label) so it pastes cleanly into
    // `crabcc memory get` / `forget` on the CLI.
    let id_for_copy = node.id.clone();
    let id_tooltip: SharedString =
        SharedString::from(format!("Click to copy \u{201C}{}\u{201D}", node.id));
    let id_chip = div()
        .id("k-graph-detail-id-copy")
        .px_1()
        .rounded_md()
        .text_color(primary)
        .text_xs()
        .cursor_pointer()
        .hover(move |s| s.bg(border))
        .tooltip(move |window, cx| Tooltip::new(id_tooltip.clone()).build(window, cx))
        .child(SharedString::from(node.id.to_string()))
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            cx.stop_propagation();
            cx.write_to_clipboard(ClipboardItem::new_string(id_for_copy.to_string()));
        });
    let subline = h_flex()
        .gap_1()
        .text_color(muted)
        .text_xs()
        .child(SharedString::from(format!(
            "wing {} · {} bytes ·",
            node.kind, node.len
        )))
        .child(id_chip);

    let header = v_flex()
        .gap_0p5()
        .child(
            div()
                .text_color(foreground)
                .child(SharedString::from(node.title.to_string())),
        )
        .child(subline)
        .child(knowledge_link);

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
pub(super) fn render_canvas(
    layout: Option<&Layout>,
    node_ids: &[SharedString],
    nodes: &[MemoryGraphNode],
    selected: Option<&SharedString>,
    zoom: f32,
    pan: (f32, f32),
    at_identity: bool,
    bounds_share: Arc<Mutex<Option<Bounds<Pixels>>>>,
    view: Entity<KnowledgeGraphRoute>,
    foreground: Hsla,
    muted: Hsla,
    border: Hsla,
    secondary: Hsla,
    primary: Hsla,
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

    // Hub labels: pick the highest-degree drawers and render their
    // title under the pill. Single-degree drawers vastly outnumber
    // hubs, so labelling them all turns the canvas into a wall of
    // text — capping at `MAX_HUB_LABELS` with a `MIN_HUB_DEGREE`
    // floor keeps the labels useful (shows the dense knots) without
    // overwhelming the visual at typical density.
    let hubs = pick_hub_labels(layout, nodes);

    // Neighbour set for the active selection — mirrors the relations
    // graph's `GraphView::neighbours` pattern. Empty when nothing is
    // selected, in which case paint takes the unfocused path (every
    // pill at full intensity).
    let neighbours = neighbours_of(selected_idx, &layout.edge_indices);

    let layout_clone = layout.clone();
    let node_tones_clone = node_tones.clone();

    let canvas_el = canvas(
        move |_bounds, _, _| (),
        move |bounds: Bounds<Pixels>, _, window, cx| {
            if let Ok(mut guard) = bounds_share.lock() {
                *guard = Some(bounds);
            }
            paint_k_graph(
                bounds,
                &layout_clone,
                &node_tones_clone,
                selected_idx,
                &neighbours,
                &hubs,
                zoom,
                pan,
                foreground,
                muted,
                window,
                cx,
            );
        },
    )
    .size_full();

    // The interactive container owns the press / move / release set
    // and scroll wheel — same shape as `routes::graph::GraphView`'s
    // canvas wrapper. Mouse-down records drag origin; on-up either
    // commits the click (no movement) or drops the gesture.
    let entity_for_down = view.clone();
    let entity_for_move = view.clone();
    let entity_for_up = view.clone();
    let entity_for_up_out = view.clone();
    let entity_for_scroll = view.clone();
    let canvas_container = div()
        .id("k-graph-canvas")
        .size_full()
        .child(canvas_el)
        .on_mouse_down(MouseButton::Left, move |event, _, cx| {
            let pos = event.position;
            entity_for_down.update(cx, |this, cx| {
                this.drag_start(pos);
                cx.notify();
            });
        })
        .on_mouse_move(move |event, _, cx| {
            // Hover moves don't matter — only relevant while held.
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
                    this.handle_canvas_click(click_pos);
                }
                cx.notify();
            });
        })
        .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
            // Mouse released outside the canvas — drop the gesture
            // without firing a click.
            entity_for_up_out.update(cx, |this, cx| {
                this.drag = None;
                cx.notify();
            });
        })
        .on_scroll_wheel(move |event, _, cx| {
            // 16px line-height proxy — macOS trackpads ignore it
            // (already pixel-precise), legacy line-based wheels use it.
            let dy = f32::from(event.delta.pixel_delta(px(16.0)).y);
            if dy != 0.0 {
                entity_for_scroll.update(cx, |this, cx| {
                    this.adjust_zoom(dy);
                    cx.notify();
                });
            }
        });

    // ── Header row: counts, zoom %, reset-view ──────────────────────
    let mut header = h_flex()
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
        );
    if (zoom - 1.0).abs() > f32::EPSILON {
        header = header.child(
            div()
                .text_color(muted)
                .text_xs()
                .child(SharedString::from(format!("· {:.0}% zoom", zoom * 100.0))),
        );
    }
    if !at_identity {
        let entity_for_reset = view.clone();
        header = header.child(
            div()
                .id("k-graph-reset-view")
                .ml_2()
                .px_2()
                .py_0p5()
                .border_1()
                .border_color(border)
                .rounded_md()
                .text_color(primary)
                .text_xs()
                .cursor_pointer()
                .hover(move |s| s.border_color(primary))
                .tooltip(|window, cx| {
                    Tooltip::new("Reset zoom + pan to identity").build(window, cx)
                })
                .child(SharedString::new_static("Reset view"))
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    entity_for_reset.update(cx, |this, cx| {
                        this.reset_view();
                        cx.notify();
                    });
                }),
        );
    }

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
        .child(header)
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

/// Maximum number of hub labels painted on the canvas. Beyond this
/// the labels overlap each other on dense graphs — the wing-grouped
/// list below already covers full disclosure.
const MAX_HUB_LABELS: usize = 8;
/// Minimum degree (incoming + outgoing edges) a node needs before
/// it earns a label. A drawer with one or two edges isn't really a
/// hub; labelling it just adds noise. 3+ matches the graph-orphans
/// CLI heuristic.
const MIN_HUB_DEGREE: usize = 3;
/// Title font size used for hub labels. Smaller than the route's body
/// (text-xs ≈ 11) so labels don't compete with the pill itself —
/// reads as annotation, not a primary control.
const HUB_LABEL_FONT_SIZE: f32 = 9.0;
/// Vertical gap between the bottom of a hub pill and the top of its
/// label. Kept small (2 px) so the label visually anchors to the
/// pill instead of looking like an unrelated row underneath.
const HUB_LABEL_GAP_PX: f32 = 2.0;
/// Maximum hub-label character count. Drawer titles can run long
/// (the API caps at 80 chars); on the canvas a long label crowds
/// neighbouring pills, so we ellipsize at this width.
const HUB_LABEL_MAX_CHARS: usize = 28;

/// One hub label scheduled for paint — pre-resolved layout index +
/// truncated title so the paint closure does no allocations beyond
/// the per-glyph shape pass.
#[derive(Clone)]
pub(super) struct HubLabel {
    index: usize,
    title: SharedString,
}

/// Apply a wheel tick to `current` zoom and clamp into bounds.
/// Exponential mapping (so a single scroll click multiplies /
/// divides instead of adding). Pure so the tests can drive it
/// without standing up an `Entity<AppState>`.
pub(super) fn next_zoom(current: f32, dy_px: f32) -> f32 {
    let factor = (SCROLL_K * dy_px).exp();
    (current * factor).clamp(MIN_ZOOM, MAX_ZOOM)
}

/// True iff zoom == 1 and pan == (0, 0) within float tolerance.
pub(super) fn is_identity_view(zoom: f32, pan: (f32, f32)) -> bool {
    (zoom - 1.0).abs() <= f32::EPSILON && pan.0.abs() <= f32::EPSILON && pan.1.abs() <= f32::EPSILON
}

/// Layout indices adjacent to `selected` in the undirected edge
/// list. Returns an empty set when nothing is selected so the paint
/// path treats it as "no halo, no dimming."
pub(super) fn neighbours_of(selected: Option<usize>, edges: &[(usize, usize)]) -> HashSet<usize> {
    let Some(i) = selected else {
        return HashSet::new();
    };
    edges
        .iter()
        .filter_map(|&(a, b)| {
            if a == i {
                Some(b)
            } else if b == i {
                Some(a)
            } else {
                None
            }
        })
        .collect()
}

/// Pick the up-to-`MAX_HUB_LABELS` highest-degree drawers (by edge
/// count from `layout.edge_indices`) with degree ≥ `MIN_HUB_DEGREE`,
/// returning their layout index + a shortened title ready for paint.
pub(super) fn pick_hub_labels(layout: &Layout, nodes: &[MemoryGraphNode]) -> Vec<HubLabel> {
    if nodes.is_empty() || layout.positions.is_empty() {
        return Vec::new();
    }
    let mut degrees: Vec<usize> = vec![0; layout.positions.len()];
    for &(a, b) in &layout.edge_indices {
        if let Some(d) = degrees.get_mut(a) {
            *d += 1;
        }
        if let Some(d) = degrees.get_mut(b) {
            *d += 1;
        }
    }
    let mut ranked: Vec<(usize, usize)> = degrees
        .iter()
        .enumerate()
        .filter_map(|(i, &d)| (d >= MIN_HUB_DEGREE).then_some((i, d)))
        .collect();
    // Highest degree first; stable ordering by index for ties keeps
    // the label set deterministic across re-renders.
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.truncate(MAX_HUB_LABELS);
    ranked
        .into_iter()
        .filter_map(|(i, _)| {
            let title = nodes.get(i)?.title.as_ref();
            Some(HubLabel {
                index: i,
                title: SharedString::from(truncate_label(title)),
            })
        })
        .collect()
}

/// Trim `s` to fit within `HUB_LABEL_MAX_CHARS`, appending an
/// ellipsis when truncated. Operates on chars (not bytes) so
/// multi-byte glyphs don't get sliced mid-codepoint.
pub(super) fn truncate_label(s: &str) -> String {
    let count = s.chars().count();
    if count <= HUB_LABEL_MAX_CHARS {
        return s.to_string();
    }
    let mut out: String = s.chars().take(HUB_LABEL_MAX_CHARS - 1).collect();
    out.push('…');
    out
}

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
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_k_graph(
    bounds: Bounds<Pixels>,
    layout: &Layout,
    node_tones: &[Hsla],
    selected: Option<usize>,
    neighbours: &HashSet<usize>,
    hubs: &[HubLabel],
    zoom: f32,
    pan: (f32, f32),
    foreground: Hsla,
    muted: Hsla,
    window: &mut Window,
    cx: &mut App,
) {
    let ox = f32::from(bounds.origin.x);
    let oy = f32::from(bounds.origin.y);
    let w = f32::from(bounds.size.width);
    let h = f32::from(bounds.size.height);

    // Zoom + pan transform: visible = 0.5 + (orig - 0.5) * zoom + pan.
    // Inverse lives in `hit_test`. Out-of-bounds paint calls clip
    // automatically — gpui doesn't error.
    let to_px = |(ux, uy): (f32, f32)| {
        let evx = 0.5 + (ux - 0.5) * zoom + pan.0;
        let evy = 0.5 + (uy - 0.5) * zoom + pan.1;
        point(px(ox + evx * w), px(oy + evy * h))
    };

    // Edges first so pills paint on top. Dashed via manual segment
    // walk — see `paint_dashed_edge`.
    //
    // When a node is selected, edges incident to it brighten and
    // non-incident edges dim further — same neighbour-halo idea as
    // the relations graph (`routes/graph.rs`) but adapted to the
    // dashed pen.
    let edge_color = with_alpha(muted, 0.45);
    let edge_color_dim = with_alpha(muted, 0.18);
    let edge_color_hot = with_alpha(foreground, 0.7);
    for &(a, b) in &layout.edge_indices {
        if let (Some(p1), Some(p2)) = (layout.positions.get(a), layout.positions.get(b)) {
            // Convert unit-coord endpoints to canvas pixels here so
            // the dash walk operates in pixel space (consistent
            // dash period regardless of canvas size).
            let p1_px = to_px(*p1);
            let p2_px = to_px(*p2);
            let color = match selected {
                Some(s) if a == s || b == s => edge_color_hot,
                Some(_) => edge_color_dim,
                None => edge_color,
            };
            paint_dashed_edge(p1_px, p2_px, color, window);
        }
    }

    // Pills. Each gets its wing colour at base alpha; the selected
    // pill paints again with a 2-px foreground ring (drawn as a
    // slightly larger filled quad below the inner fill). Non-neighbour
    // pills dim when a selection is active so the eye traces the
    // selected node's connection ring without the rest of the
    // canvas competing.
    let half_w = PILL_WIDTH * 0.5;
    let half_h = PILL_HEIGHT * 0.5;
    for (i, &(ux, uy)) in layout.positions.iter().enumerate() {
        let centre = to_px((ux, uy));
        let mut tone = node_tones.get(i).copied().unwrap_or(muted);
        let is_selected = Some(i) == selected;
        let is_neighbour = neighbours.contains(&i);
        if selected.is_some() && !is_selected && !is_neighbour {
            tone = with_alpha(tone, 0.25);
        }
        if is_selected {
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

    // Hub labels last so they paint over the pills (the title sits
    // just below each hub pill, centred horizontally on the pill).
    // Foreground at 75% alpha — readable but doesn't compete with
    // selection-ring brightness. Non-neighbour hubs dim alongside
    // their pills when something else is selected.
    let label_color = with_alpha(foreground, 0.75);
    let label_color_dim = with_alpha(foreground, 0.25);
    let font = window.text_style().font();
    for hub in hubs {
        let Some((ux, uy)) = layout.positions.get(hub.index).copied() else {
            continue;
        };
        let centre = to_px((ux, uy));
        let centre_x = f32::from(centre.x);
        let centre_y = f32::from(centre.y);
        let is_selected = Some(hub.index) == selected;
        let is_neighbour = neighbours.contains(&hub.index);
        let color = if selected.is_some() && !is_selected && !is_neighbour {
            label_color_dim
        } else {
            label_color
        };
        let run = TextRun {
            len: hub.title.len(),
            font: font.clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = window.text_system().shape_line(
            hub.title.clone(),
            px(HUB_LABEL_FONT_SIZE),
            &[run],
            None,
        );
        let label_w = f32::from(line.width());
        let origin = point(
            px(centre_x - label_w * 0.5),
            px(centre_y + half_h + HUB_LABEL_GAP_PX),
        );
        let _ = line.paint(
            origin,
            px(HUB_LABEL_FONT_SIZE),
            gpui::TextAlign::Left,
            None,
            window,
            cx,
        );
    }
}

pub(super) fn with_alpha(c: Hsla, a: f32) -> Hsla {
    Hsla { a, ..c }
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
pub(super) fn paint_dashed_edge(
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
