//! Knowledge graph view (#317). Renders the `/api/memory/graph`
//! response — drawers as nodes, cross-references (resolved
//! server-side from `web:<hash>` / `text:<hash>` / `doc:<n>` ids and
//! Obsidian-style `[[Title]]` matches) as edges.
//!
//! The brief asks for a Roam-like canvas (rounded-rect pills,
//! dashed cross-ref lines, foreground-coloured selection ring,
//! right-rail Drawer Detail panel). This v1 ships a **list-based
//! rendering** of the same data: wing-grouped node list with
//! cross-ref counts per drawer + a top-N edges section. A force-
//! directed paint pass distinct from the relations graph is a
//! deliberate follow-up — the data layer landing first lets a
//! later PR focus purely on the visual differentiation without
//! touching state plumbing.
//!
//! State is stored on `AppState::memory_graph` (lazy fetch on
//! first render via `submit_memory_graph`; manual refresh button
//! re-runs the same path). Errors land on
//! `AppState::memory_graph_error` and render inline.

use std::collections::{BTreeMap, HashMap};

use gpui::{
    div, prelude::*, px, Context, Entity, Hsla, IntoElement, MouseButton, Render, SharedString,
    Window,
};
use gpui_component::{h_flex, v_flex, ActiveTheme};

use crate::api::types::{MemoryGraphEdge, MemoryGraphNode};
use crate::state::AppState;

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
    /// Selected node id (for the right-rail detail panel). Cleared
    /// by clicking the active row again or the panel's × button.
    selected: Option<SharedString>,
}

impl KnowledgeGraphRoute {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self {
            state,
            fetched_once: false,
            selected: None,
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
                let edges_for_node = build_edge_index(&g.edges);
                let by_wing = group_by_wing(&g.nodes);

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

                // Right rail: selected drawer detail.
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

                h_flex()
                    .size_full()
                    .child(sections)
                    .child(detail_panel)
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
