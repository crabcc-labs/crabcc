// Pure unit tests for the knowledge store reducer.

import { describe, expect, it } from "bun:test";
import {
  flattenEdges,
  fromSnapshot,
  inferSeeds,
  neighborhoodOf,
  searchHighlight,
  titleIndex,
} from "./store";
import type { KnowledgeSnapshot } from "./types";

const SNAP: KnowledgeSnapshot = {
  nodes: [
    { id: "doc:1", title: "Architecture", kind: "project", ts: 1, len: 100 },
    { id: "doc:2", title: "Roadmap", kind: "project", ts: 2, len: 50 },
    { id: "doc:3", title: "Notes", kind: "project", ts: 3, len: 25 },
    { id: "doc:4", title: "Standalone", kind: "project", ts: 4, len: 10 },
  ],
  edges: [
    { src: "doc:1", dst: "doc:2", via: "ref" },
    { src: "doc:1", dst: "doc:3", via: "wiki" },
    { src: "doc:2", dst: "doc:3", via: "ref" },
    // Edge to a node outside the snapshot must be filtered out.
    { src: "doc:1", dst: "doc:99", via: "ref" },
  ],
  stats: { drawers: 4, edges: 4, embeddings: false },
};

describe("fromSnapshot", () => {
  it("keeps every captured drawer (incl. zero-degree)", () => {
    const layout = fromSnapshot(SNAP);
    expect(layout.nodes.map((n) => n.id).sort()).toEqual([
      "doc:1",
      "doc:2",
      "doc:3",
      "doc:4",
    ]);
  });

  it("drops edges that point outside the captured node set", () => {
    const layout = fromSnapshot(SNAP);
    expect(flattenEdges(layout.links)).toHaveLength(3);
  });

  it("returns SimLink object refs (not strings) so d3-force is happy", () => {
    const layout = fromSnapshot(SNAP);
    for (const l of layout.links) {
      expect(typeof l.source).toBe("object");
      expect(typeof l.target).toBe("object");
    }
  });

  it("dedupes identical edges defensively", () => {
    const dup: KnowledgeSnapshot = {
      ...SNAP,
      edges: [
        { src: "doc:1", dst: "doc:2", via: "ref" },
        { src: "doc:1", dst: "doc:2", via: "wiki" },
      ],
    };
    expect(fromSnapshot(dup).links).toHaveLength(1);
  });
});

describe("inferSeeds", () => {
  it("picks the highest-degree drawers as hubs", () => {
    const seeds = inferSeeds(SNAP);
    // doc:1 is the only triple-degree node; it must be a seed.
    expect(seeds.has("doc:1")).toBe(true);
  });

  it("ignores zero-degree drawers", () => {
    const seeds = inferSeeds(SNAP);
    expect(seeds.has("doc:4")).toBe(false);
  });
});

describe("searchHighlight", () => {
  const layout = fromSnapshot(SNAP);
  const titles = titleIndex(SNAP);

  it("returns null for an empty query", () => {
    expect(searchHighlight("", layout.nodes, titles)).toBe(null);
  });

  it("matches by id substring", () => {
    const hit = searchHighlight("doc:1", layout.nodes, titles);
    expect(hit !== null && hit.has("doc:1")).toBe(true);
  });

  it("matches by title substring (case-insensitive)", () => {
    const hit = searchHighlight("road", layout.nodes, titles);
    expect(hit !== null && hit.has("doc:2")).toBe(true);
  });
});

describe("neighborhoodOf", () => {
  it("includes the seed and every direct neighbor", () => {
    const layout = fromSnapshot(SNAP);
    const nbhd = neighborhoodOf(layout.links, "doc:1");
    expect(nbhd.has("doc:1")).toBe(true);
    expect(nbhd.has("doc:2")).toBe(true);
    expect(nbhd.has("doc:3")).toBe(true);
    expect(nbhd.has("doc:4")).toBe(false);
  });
});
