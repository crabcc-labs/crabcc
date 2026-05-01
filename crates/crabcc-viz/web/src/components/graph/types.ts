// Wire types match the Rust handlers in `crates/crabcc-viz/src/api/`.
// Keep them duplicated (rather than imported from api.gen.ts) because
// the OpenAPI for /api/graph + /api/seed-graph is intentionally opaque
// (`additionalProperties: true`) — the generated alias is `unknown`.

import type { SimulationNodeDatum } from "d3-force";

export type WireNode = { id: string; depth: number };
export type WireEdge = { src: string; dst: string };

export type SeedSnapshot = {
  nodes: WireNode[];
  edges: WireEdge[];
  seeds: string[];
};

export type GraphSnapshot = {
  root: string;
  dir: "callers" | "callees";
  depth: number;
  truncated: boolean;
  nodes: WireNode[];
  edges: WireEdge[];
};

/** Live graph node — d3-force mutates `x/y/vx/vy/index` on it in place. */
export interface SimNode extends SimulationNodeDatum {
  id: string;
  depth: number;
  /** Was this node a seed in the original snapshot? Stays true after expansion. */
  isSeed: boolean;
  /** Has the user already expanded this node's callers? Used to dim the button. */
  expandedCallers?: boolean;
  expandedCallees?: boolean;
  /** Pin position so re-warmup keeps existing nodes near where the user expects. */
  fx?: number | null;
  fy?: number | null;
}

export interface SimLink {
  source: SimNode;
  target: SimNode;
  /** Was this link in the original snapshot or added later? Used for fade-in. */
  added?: boolean;
}

export type Layout = { nodes: SimNode[]; links: SimLink[] };
