// Pure tests for the activity-panel store. No DOM, no React.

import { describe, expect, it } from "bun:test";
import {
  asHitRows,
  bucketFor,
  computeWindow,
  filterHits,
  groupByOp,
  pinId,
  relativeTime,
  withHeaders,
} from "./store";
import type { ActivityHit } from "./types";

const NOW = 1_700_000_000;

function hit(partial: Partial<ActivityHit> & { ts: number; op: string; query: string }): ActivityHit {
  return { count: 1, ...partial };
}

describe("filterHits", () => {
  const hits: ActivityHit[] = [
    hit({ ts: NOW, op: "sym", query: "Store" }),
    hit({ ts: NOW - 5, op: "callers", query: "Store::open" }),
    hit({ ts: NOW - 10, op: "refs", query: "FooBar" }),
  ];

  it("returns input by reference when filter is empty", () => {
    expect(filterHits(hits, { text: "", op: null, agent: null })).toBe(hits);
  });

  it("filters by case-insensitive substring on query", () => {
    const out = filterHits(hits, { text: "store", op: null, agent: null });
    expect(out.map((h) => h.query)).toEqual(["Store", "Store::open"]);
  });

  it("filters by exact op", () => {
    const out = filterHits(hits, { text: "", op: "callers", agent: null });
    expect(out).toHaveLength(1);
    expect(out[0].op).toBe("callers");
  });

  it("text also matches op name", () => {
    const out = filterHits(hits, { text: "ref", op: null, agent: null });
    expect(out.map((h) => h.op)).toEqual(["refs"]);
  });

  it("filters by agent when present", () => {
    const tagged: ActivityHit[] = [
      { ...hits[0], agent: "claude-A" } as ActivityHit & { agent: string },
      { ...hits[1], agent: "claude-B" } as ActivityHit & { agent: string },
    ];
    const out = filterHits(tagged, { text: "", op: null, agent: "claude-A" });
    expect(out).toHaveLength(1);
  });
});

describe("groupByOp", () => {
  it("collapses consecutive same-op runs into a group row", () => {
    const hits: ActivityHit[] = [
      hit({ ts: NOW, op: "sym", query: "A" }),
      hit({ ts: NOW - 1, op: "sym", query: "B" }),
      hit({ ts: NOW - 2, op: "sym", query: "C" }),
      hit({ ts: NOW - 3, op: "callers", query: "D" }),
    ];
    const rows = groupByOp(hits, new Set());
    expect(rows).toHaveLength(2);
    expect(rows[0].kind).toBe("group");
    if (rows[0].kind === "group") {
      expect(rows[0].count).toBe(3);
      expect(rows[0].lastQuery).toBe("A");
    }
    expect(rows[1].kind).toBe("hit");
  });

  it("does not collapse single-element runs", () => {
    const hits: ActivityHit[] = [
      hit({ ts: NOW, op: "sym", query: "A" }),
      hit({ ts: NOW - 1, op: "callers", query: "B" }),
    ];
    const rows = groupByOp(hits, new Set());
    expect(rows.every((r) => r.kind === "hit")).toBe(true);
  });

  it("expands group when key is in the expanded set", () => {
    const hits: ActivityHit[] = [
      hit({ ts: NOW, op: "sym", query: "A" }),
      hit({ ts: NOW - 1, op: "sym", query: "B" }),
    ];
    const closed = groupByOp(hits, new Set());
    const groupKey = closed[0].key;
    const opened = groupByOp(hits, new Set([groupKey]));
    // 1 group row + 2 child hits.
    expect(opened).toHaveLength(3);
    expect(opened[0].kind).toBe("group");
    expect(opened[1].kind).toBe("hit");
    expect(opened[2].kind).toBe("hit");
  });
});

describe("bucketFor / withHeaders", () => {
  it("classifies ages correctly", () => {
    expect(bucketFor(NOW, NOW)).toBe("just now");
    expect(bucketFor(NOW - 30, NOW)).toBe("just now");
    expect(bucketFor(NOW - 120, NOW)).toBe("1m ago");
    expect(bucketFor(NOW - 800, NOW)).toBe("5m ago");
    expect(bucketFor(NOW - 1000, NOW)).toBe("15m ago");
    expect(bucketFor(NOW - 60_000, NOW)).toBe("6h ago");
    expect(bucketFor(NOW - 30 * 86400, NOW)).toBe("older");
  });

  it("inserts a header before each new bucket", () => {
    const rows = asHitRows([
      hit({ ts: NOW, op: "sym", query: "A" }),
      hit({ ts: NOW - 30, op: "sym", query: "B" }),
      hit({ ts: NOW - 200, op: "sym", query: "C" }),
    ]);
    const out = withHeaders(rows, [], NOW);
    // Two distinct buckets ("just now" then "1m ago"), so 2 headers + 3 rows.
    expect(out.filter((r) => r.kind === "header")).toHaveLength(2);
    expect(out).toHaveLength(5);
  });

  it("places pinned rows under a 'pinned' header at the top", () => {
    const pinned: ActivityHit[] = [hit({ ts: NOW - 5, op: "sym", query: "P" })];
    const rest = asHitRows([hit({ ts: NOW, op: "callers", query: "Q" })]);
    const out = withHeaders(rest, pinned, NOW);
    expect(out[0].kind).toBe("header");
    if (out[0].kind === "header") expect(out[0].label).toBe("pinned");
    expect(out[1].kind).toBe("hit");
    if (out[1].kind === "hit") expect(out[1].pinned).toBe(true);
  });
});

describe("relativeTime", () => {
  it("returns coarse human-friendly strings", () => {
    expect(relativeTime(NOW, NOW)).toBe("now");
    expect(relativeTime(NOW - 5, NOW)).toBe("5s ago");
    expect(relativeTime(NOW - 65, NOW)).toBe("1m ago");
    expect(relativeTime(NOW - 7200, NOW)).toBe("2h ago");
    expect(relativeTime(NOW - 2 * 86400, NOW)).toBe("2d ago");
  });
});

describe("computeWindow", () => {
  it("returns empty window when total is 0", () => {
    const w = computeWindow(0, 0, 800, 24, 4);
    expect(w).toEqual({ start: 0, end: 0, padTop: 0, padBottom: 0 });
  });

  it("renders only the visible slice + overscan", () => {
    const w = computeWindow(1000, 0, 240, 24, 4);
    expect(w.start).toBe(0);
    // 240/24 = 10 visible + 4 overscan top + 4 overscan bottom = 18 — but
    // start=0 clips top overscan, so end = 10 + 4 + 4 = 18.
    expect(w.end).toBe(18);
    expect(w.padTop).toBe(0);
    expect(w.padBottom).toBe((1000 - 18) * 24);
  });

  it("scrolls into the middle of the list", () => {
    const w = computeWindow(1000, 480, 240, 24, 4);
    // scrollTop=480 → row 20. Minus 4 overscan = 16.
    expect(w.start).toBe(16);
    expect(w.padTop).toBe(16 * 24);
    expect(w.end - w.start).toBeGreaterThan(9);
  });

  it("clips end at total", () => {
    const w = computeWindow(20, 10000, 240, 24, 4);
    expect(w.end).toBe(20);
    expect(w.padBottom).toBe(0);
  });
});

describe("pinId", () => {
  it("is stable across identical hits", () => {
    const a = hit({ ts: NOW, op: "sym", query: "X" });
    const b = hit({ ts: NOW, op: "sym", query: "X" });
    expect(pinId(a)).toBe(pinId(b));
  });
  it("differs when ts/op/query differ", () => {
    const a = hit({ ts: NOW, op: "sym", query: "X" });
    const b = hit({ ts: NOW, op: "sym", query: "Y" });
    expect(pinId(a) === pinId(b)).toBe(false);
  });
});
