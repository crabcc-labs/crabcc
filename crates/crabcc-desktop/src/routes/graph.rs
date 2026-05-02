//! Relations graph viewer — A.5 (Milestone 2).
//!
//! A non-interactive `gpui::canvas` view that paints the seed-graph
//! returned by `/api/seed-graph`:
//!
//!   * Edges are stroked thin lines (PathBuilder::stroke).
//!   * Nodes are filled quads with full corner_radii (≈ circles).
//!
//! Layout runs once per `GraphSnapshot` identity (size of the node
//! set used as a cheap fingerprint). Resizing the window doesn't
//! re-layout — positions are stored in unit coords and scaled into
//! the live canvas bounds at paint time. Zoom / pan / hit-test are
//! deferred to A.5.1.

use gpui::{
    canvas, div, point, prelude::*, px, Bounds, Context, Entity, Hsla, IntoElement, PathBuilder,
    Pixels, Render, SharedString, Window,
};
use gpui_component::{v_flex, ActiveTheme};

use crate::api::types::GraphSnapshot;
use crate::graph_layout::{self, Layout};
use crate::state::AppState;

/// One painted blob per node — small enough to read overlapping
/// clusters, large enough to see at a glance.
const NODE_RADIUS: f32 = 4.0;
const EDGE_WIDTH: f32 = 1.0;

pub struct GraphView {
    state: Entity<AppState>,
    layout: Option<Layout>,
    /// Snapshot fingerprint at the time `layout` was computed —
    /// used to invalidate when the prefetch result lands.
    layout_fingerprint: usize,
}

impl GraphView {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self {
            state,
            layout: None,
            layout_fingerprint: 0,
        }
    }

    fn ensure_layout(&mut self, snapshot: &GraphSnapshot) -> &Layout {
        let fp = snapshot.nodes.len() ^ (snapshot.edges.len() << 16);
        if self.layout.is_none() || self.layout_fingerprint != fp {
            self.layout = Some(graph_layout::run(snapshot));
            self.layout_fingerprint = fp;
        }
        self.layout.as_ref().expect("set above")
    }
}

impl Render for GraphView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;

        // Read-only borrow ends before we clone what the canvas closure
        // needs to outlive the borrow checker (snapshot enters the
        // 'static-bounded paint closure via `Arc`-style move).
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
                // Layout cached on the view; recomputed only when the
                // snapshot fingerprint changes.
                let layout = self.ensure_layout(&snapshot).clone();
                let node_color = cx.theme().primary;
                let edge_color = with_alpha(cx.theme().muted_foreground, 0.45);
                canvas(
                    move |_bounds, _, _| (),
                    move |bounds: Bounds<Pixels>, _, window, _| {
                        paint_graph(bounds, &layout, edge_color, node_color, window);
                    },
                )
                .size_full()
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
                div()
                    .text_sm()
                    .child(SharedString::new_static("Relations graph")),
            )
            .child(body)
    }
}

fn paint_graph(
    bounds: Bounds<Pixels>,
    layout: &Layout,
    edge_color: Hsla,
    node_color: Hsla,
    window: &mut Window,
) {
    let ox = f32::from(bounds.origin.x);
    let oy = f32::from(bounds.origin.y);
    let w = f32::from(bounds.size.width);
    let h = f32::from(bounds.size.height);

    let to_px = |(ux, uy): (f32, f32)| {
        point(px(ox + ux * w), px(oy + uy * h))
    };

    // Edges first so nodes render on top.
    for &(a, b) in &layout.edge_indices {
        if let (Some(p1), Some(p2)) =
            (layout.positions.get(a), layout.positions.get(b))
        {
            let mut pb = PathBuilder::stroke(px(EDGE_WIDTH));
            pb.move_to(to_px(*p1));
            pb.line_to(to_px(*p2));
            if let Ok(path) = pb.build() {
                window.paint_path(path, edge_color);
            }
        }
    }

    // Nodes — small filled circles via paint_quad + max corner radii.
    let r = NODE_RADIUS;
    for pos in &layout.positions {
        let centre = to_px(*pos);
        let bounds = Bounds {
            origin: point(centre.x - px(r), centre.y - px(r)),
            size: gpui::size(px(r * 2.0), px(r * 2.0)),
        };
        window.paint_quad(
            gpui::fill(bounds, node_color).corner_radii(gpui::Corners::all(px(r))),
        );
    }
}

fn with_alpha(c: Hsla, a: f32) -> Hsla {
    Hsla { a, ..c }
}
