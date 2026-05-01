// Pure unit tests for the graph store reducer + helpers. Bun's
// runtime has no DOM; everything here is deliberately framework-free.

import { describe, expect, it } from "bun:test";
import { degrees, fromSeed, mergeExpansion, neighborhood } from "./store";
import type { GraphSnapshot, SeedSnapshot } from "./types";

const SEED: SeedSnapshot = {
  nodes: [
    { id: "A", depth: 0 },
    { id: "B", depth: 1 },
    { id: "C", depth: 1 },
    { id: "Iso", depth: 2 }, // zero-degree, must be dropped.
  ],
  edges: [
    { src: "A", dst: "B" },
    { src: "A", dst: "C" },
  ],
  seeds: ["A"],
};

describe("fromSeed", () => {
  it("drops zero-degree nodes and keeps connected ones", () => {
    const layout = fromSeed(SEED);
    expect(layout.nodes.map((n) => n.id).sort()).toEqual(["A", "B", "C"]);
    expect(layout.nodes.find((n) => n.id === "A")?.isSeed).toBe(true);
    expect(layout.nodes.find((n) => n.id === "B")?.isSeed).toBe(false);
  });

  it("returns SimLink refs (not strings) so d3-force is happy", () => {
    const layout = fromSeed(SEED);
    expect(layout.links).toHaveLength(2);
    for (const l of layout.links) {
      expect(typeof l.source).toBe("object");
      expect(typeof l.target).toBe("object");
    }
  });

  it("preserves wire-edge identity through SimNode refs", () => {
    const layout = fromSeed(SEED);
    const a = layout.nodes.find((n) => n.id === "A")!;
    const b = layout.nodes.find((n) => n.id === "B")!;
    const ab = layout.links.find((l) => l.source.id === "A" && l.target.id === "B");
    expect(ab).toBeDefined();
    // Same object identity — required for d3-force to mutate `index`/`x`/`y` in place.
    expect(ab!.source).toBe(a);
    expect(ab!.target).toBe(b);
  });
});

describe("mergeExpansion", () => {
  const expansion: GraphSnapshot = {
    root: "B",
    dir: "callees",
    depth: 2,
    truncated: false,
    nodes: [
      { id: "B", depth: 0 },
      { id: "D", depth: 1 },
      { id: "E", depth: 1 },
    ],
    edges: [
      { src: "B", dst: "D" },
      { src: "B", dst: "E" },
    ],
  };

  it("merges new nodes without losing existing identities", () => {
    const layout = fromSeed(SEED);
    // Pretend the layout has settled — give A,B,C real positions.
    for (const [i, n] of layout.nodes.entries()) {
      n.x = i * 10;
      n.y = i * 5;
    }
    const a = layout.nodes.find((n) => n.id === "A")!;
    const merged = mergeExpansion(layout, expansion);
    expect(merged.nodes.map((n) => n.id).sort()).toEqual(["A", "B", "C", "D", "E"]);
    // A's identity is preserved (same SimNode reference + position).
    const a2 = merged.nodes.find((n) => n.id === "A")!;
    expect(a2).toBe(a);
    expect(a2.x).toBe(0);
  });

  it("flags the expanded direction on the root", () => {
    const layout = fromSeed(SEED);
    const merged = mergeExpansion(layout, expansion);
    const b = merged.nodes.find((n) => n.id === "B")!;
    expect(b.expandedCallees).toBe(true);
    expect(b.expandedCallers).toBeUndefined();
  });

  it("keeps the lower depth when re-merging the same node", () => {
    const layout = fromSeed(SEED);
    // C is depth 1 in the seed. Pretend an expansion arrives that
    // reports C at depth 3 — we should not regress.
    const merged = mergeExpansion(layout, {
      ...expansion,
      root: "Q",
      nodes: [
        { id: "Q", depth: 0 },
        { id: "C", depth: 3 },
      ],
      edges: [{ src: "Q", dst: "C" }],
    });
    expect(merged.nodes.find((n) => n.id === "C")?.depth).toBe(1);
  });

  it("dedupes edges across calls", () => {
    const layout = fromSeed(SEED);
    const merged = mergeExpansion(layout, {
      ...expansion,
      nodes: [
        { id: "A", depth: 0 },
        { id: "B", depth: 1 },
      ],
      edges: [{ src: "A", dst: "B" }], // already present
    });
    const ab = merged.links.filter(
      (l) => l.source.id === "A" && l.target.id === "B",
    );
    expect(ab).toHaveLength(1);
  });

  it("places fresh nodes near the expansion root", () => {
    const layout = fromSeed(SEED);
    const b = layout.nodes.find((n) => n.id === "B")!;
    b.x = 100;
    b.y = 200;
    const merged = mergeExpansion(layout, expansion);
    const d = merged.nodes.find((n) => n.id === "D")!;
    // Within ±10 of B's position (jitter range = 20 wide).
    expect(Math.abs((d.x ?? 0) - 100)).toBeLessThan(15);
    expect(Math.abs((d.y ?? 0) - 200)).toBeLessThan(15);
  });
});

describe("degrees", () => {
  it("counts in/out separately on a directed link list", () => {
    const layout = fromSeed(SEED);
    const a = degrees(layout.links, "A");
    expect(a).toEqual({ inDeg: 0, outDeg: 2 });
    const b = degrees(layout.links, "B");
    expect(b).toEqual({ inDeg: 1, outDeg: 0 });
  });
});

describe("neighborhood", () => {
  it("returns the node + its 1-hop neighbors", () => {
    const layout = fromSeed(SEED);
    const nbhd = neighborhood(layout.links, "A");
    expect(Array.from(nbhd).sort()).toEqual(["A", "B", "C"]);
  });

  it("only contains the node itself when isolated", () => {
    const layout = fromSeed(SEED);
    const nbhd = neighborhood(layout.links, "Z"); // never present
    expect(Array.from(nbhd)).toEqual(["Z"]);
  });
});
