// Pure transforms for the knowledge view. The reducer is split out so
// the search / hop-expansion semantics stay unit-testable without React
// or the canvas. Mirrors the call-graph store's shape on purpose —
// downstream code reuses GraphCanvas which expects SimNode/SimLink refs.

import type { KnowledgeEdge, KnowledgeNode, KnowledgeSnapshot } from "./types";
import type { Layout, SimLink, SimNode } from "../graph/types";

/**
 * Convert a wire snapshot into a force-layout-ready Layout.
 *
 * Unlike `fromSeed` for the call-graph, we keep zero-degree drawers
 * because in the knowledge graph an "isolated thought" is meaningful —
 * the user might still want to read it. We surface them as small
 * dots with `isSeed: false`. Drawers without any out-edges to/from
 * other captured drawers are still navigable in the side panel.
 */
export function fromSnapshot(snap: KnowledgeSnapshot): Layout {
  const known = new Set(snap.nodes.map((n) => n.id));
  const seedSet = inferSeeds(snap);

  // SimNode shape: depth here doubles as a hint for the depth-color
  // ramp. We mark hub-like drawers (top by combined degree) as `isSeed`
  // so they paint in the accent color and stand out on the canvas.
  const nodes: SimNode[] = snap.nodes.map((n) => ({
    id: n.id,
    depth: seedSet.has(n.id) ? 0 : 1,
    isSeed: seedSet.has(n.id),
  }));

  const byId = new Map(nodes.map((n) => [n.id, n] as const));
  const links: SimLink[] = [];
  const seen = new Set<string>();
  for (const e of snap.edges) {
    if (!known.has(e.src) || !known.has(e.dst)) continue;
    const key = `${e.src} ${e.dst}`;
    if (seen.has(key)) continue;
    seen.add(key);
    const s = byId.get(e.src);
    const t = byId.get(e.dst);
    if (!s || !t) continue;
    links.push({ source: s, target: t });
  }

  return { nodes, links };
}

/**
 * Returns the IDs of "hub" drawers — the top-N by combined in/out
 * degree. We highlight these so the user has a natural starting
 * point on first paint instead of a sea of unweighted dots.
 *
 * The cutoff (`max(3, sqrt(N))`) keeps the seed set small on a busy
 * graph (where it's a useful filter) but generous on a tiny one
 * (where we'd otherwise pick zero seeds).
 */
export function inferSeeds(snap: KnowledgeSnapshot): Set<string> {
  const degree = new Map<string, number>();
  for (const e of snap.edges) {
    degree.set(e.src, (degree.get(e.src) ?? 0) + 1);
    degree.set(e.dst, (degree.get(e.dst) ?? 0) + 1);
  }
  const ranked = Array.from(degree.entries())
    .filter(([, d]) => d > 0)
    .sort((a, b) => b[1] - a[1]);
  const cap = Math.max(3, Math.floor(Math.sqrt(snap.nodes.length || 1)));
  return new Set(ranked.slice(0, cap).map(([id]) => id));
}

/**
 * Filter a layout by a free-text query. The match is case-insensitive
 * and matches against either the drawer id or the title (looked up via
 * the `titles` lookup table the orchestrator keeps in sync). Returns
 * the set of node ids to *highlight*; unmatched nodes are dimmed but
 * stay on screen so the surrounding context isn't lost.
 */
export function searchHighlight(
  query: string,
  nodes: SimNode[],
  titles: Map<string, string>,
): Set<string> | null {
  const q = query.trim().toLowerCase();
  if (!q) return null;
  const out = new Set<string>();
  for (const n of nodes) {
    if (n.id.toLowerCase().includes(q)) {
      out.add(n.id);
      continue;
    }
    const t = titles.get(n.id);
    if (t && t.toLowerCase().includes(q)) {
      out.add(n.id);
    }
  }
  return out;
}

/**
 * 1-hop neighborhood (inclusive). Same shape as the call-graph helper
 * so we can pass the resulting set straight to GraphCanvas's
 * `highlight` prop and get the dim/bright behavior for free.
 */
export function neighborhoodOf(links: SimLink[], id: string): Set<string> {
  const out = new Set<string>([id]);
  for (const l of links) {
    if (l.source.id === id) out.add(l.target.id);
    if (l.target.id === id) out.add(l.source.id);
  }
  return out;
}

/** Build an `id -> title` lookup from the wire snapshot. */
export function titleIndex(snap: KnowledgeSnapshot): Map<string, string> {
  return new Map(snap.nodes.map((n) => [n.id, n.title]));
}

/**
 * Plain `KnowledgeEdge[]` view of a layout's links. Re-used by tests
 * that want to verify the reducer didn't drop or duplicate edges.
 */
export function flattenEdges(links: SimLink[]): KnowledgeEdge[] {
  return links.map((l) => ({ src: l.source.id, dst: l.target.id, via: "ref" }));
}

/** Re-export so the orchestrator can import everything from one place. */
export type { KnowledgeNode };
