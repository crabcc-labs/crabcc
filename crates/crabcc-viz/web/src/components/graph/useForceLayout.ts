// Synchronous d3-force settle: tick the simulation to convergence in
// one go, then stop. We never run an animation loop — once the layout
// is settled, panning/zooming uses an affine transform on the same
// fixed coordinates, which keeps the graph rock-stable.
//
// On expansion (a new Layout that includes additional nodes/links),
// existing nodes are pinned via fx/fy for the first half of the warmup
// so the previous picture barely moves while the new neighborhood
// settles; pins are then released and the simulation finishes loose.

import { useMemo } from "react";
import {
  forceCenter,
  forceCollide,
  forceLink,
  forceManyBody,
  forceSimulation,
} from "d3-force";
import type { Layout, SimLink, SimNode } from "./types";

interface Options {
  /** Width/height fed to forceCenter so the layout settles centered. */
  width: number;
  height: number;
  /** When true, pin existing positions for the first phase (expansion). */
  preservePositions: boolean;
}

export function useForceLayout(layout: Layout, opts: Options): Layout {
  const { width, height, preservePositions } = opts;
  return useMemo(() => {
    if (layout.nodes.length === 0) return layout;

    // Pin nodes that already have positions so a re-warmup nudges
    // newcomers without rotating or sliding the existing picture.
    if (preservePositions) {
      for (const n of layout.nodes) {
        if (n.x !== undefined && n.y !== undefined && n.fx === undefined) {
          n.fx = n.x;
          n.fy = n.y;
        }
      }
    }

    // d3-force mutates link source/target from string to ref on first
    // tick via the .id() accessor — but we already supply refs (set up
    // by `store.buildLinks`), so the .id() accessor is wired to handle
    // both shapes.
    const sim = forceSimulation<SimNode>(layout.nodes)
      .force(
        "link",
        forceLink<SimNode, SimLink>(layout.links)
          .id((d) => d.id)
          .distance(50)
          .strength(0.85),
      )
      .force("charge", forceManyBody<SimNode>().strength(-260))
      .force("center", forceCenter(0, 0))
      .force("collide", forceCollide<SimNode>(11))
      .alpha(preservePositions ? 0.6 : 1)
      .alphaDecay(0.04)
      .stop();

    const ticks = Math.ceil(
      Math.log(sim.alphaMin()) / Math.log(1 - sim.alphaDecay()),
    );

    // Phase 1: pinned existing nodes, freer newcomers.
    const half = preservePositions ? Math.floor(ticks * 0.6) : 0;
    for (let i = 0; i < half; i++) sim.tick();

    // Phase 2: release the pins, settle the whole graph loosely.
    if (preservePositions) {
      for (const n of layout.nodes) {
        n.fx = null;
        n.fy = null;
      }
    }
    for (let i = half; i < ticks; i++) sim.tick();

    void width;
    void height; // forceCenter already accounts for centering at (0,0).

    return layout;
  }, [layout, width, height, preservePositions]);
}
