// Pure transforms — no React, no DOM. Tested directly under bun test.
//
// The pipeline is: filter → group? → time-bucket → flatten → virtual.
// All steps are O(N) and assume the input is small (<= a few thousand);
// virtualization above is what keeps the *DOM* light, not these passes.

import type { ActivityHit, FilterState, Row } from "./types";

// ── filter ─────────────────────────────────────────────────────────────

/// `text` matches against query OR op (so users can type "sym" and see
/// only symbol lookups even without exact-match opt-in). Empty filter
/// returns the input by reference.
export function filterHits(
  hits: ActivityHit[],
  filter: FilterState,
): ActivityHit[] {
  const text = filter.text.trim().toLowerCase();
  if (!text && !filter.op && !filter.agent) return hits;
  return hits.filter((h) => {
    if (filter.op && h.op !== filter.op) return false;
    if (filter.agent) {
      const a = (h as { agent?: string }).agent;
      if (a !== filter.agent) return false;
    }
    if (text) {
      const q = (h.query ?? "").toLowerCase();
      const op = (h.op ?? "").toLowerCase();
      if (!q.includes(text) && !op.includes(text)) return false;
    }
    return true;
  });
}

// ── grouping ────────────────────────────────────────────────────────────

/// Collapse consecutive hits with the same `op`. We only collapse runs,
/// not all hits with the same op anywhere in the list — that way the
/// chronology reads top-to-bottom even after grouping.
export function groupByOp(
  hits: ActivityHit[],
  expandedKeys: ReadonlySet<string>,
): Row[] {
  const out: Row[] = [];
  let i = 0;
  while (i < hits.length) {
    const start = i;
    const op = hits[i].op;
    while (i < hits.length && hits[i].op === op) i++;
    const run = hits.slice(start, i);
    if (run.length === 1) {
      const h = run[0];
      out.push({ kind: "hit", key: hitKey(h, start), hit: h, pinned: false });
      continue;
    }
    const groupKey = `g:${op}:${run[0].ts}:${start}`;
    const expanded = expandedKeys.has(groupKey);
    out.push({
      kind: "group",
      key: groupKey,
      op,
      count: run.length,
      lastQuery: run[0].query,
      lastTs: run[0].ts,
      hits: run,
      expanded,
    });
    if (expanded) {
      run.forEach((h, idx) => {
        out.push({
          kind: "hit",
          key: `${groupKey}:${hitKey(h, idx)}`,
          hit: h,
          pinned: false,
        });
      });
    }
  }
  return out;
}

/// Pass-through when grouping is off — wraps each hit in a `Row`.
export function asHitRows(hits: ActivityHit[]): Row[] {
  return hits.map((h, i) => ({
    kind: "hit",
    key: hitKey(h, i),
    hit: h,
    pinned: false,
  }));
}

export function hitKey(h: ActivityHit, idx: number): string {
  // Index disambiguates hits that share (ts,op,query) — the activity log
  // routinely emits identical rows when an agent re-runs the same query.
  return `${h.ts}:${h.op}:${h.query}:${idx}`;
}

// ── time bucket headers ─────────────────────────────────────────────────

const BUCKETS: { secs: number; label: string }[] = [
  { secs: 60, label: "just now" },
  { secs: 5 * 60, label: "1m ago" },
  { secs: 15 * 60, label: "5m ago" },
  { secs: 60 * 60, label: "15m ago" },
  { secs: 6 * 60 * 60, label: "1h ago" },
  { secs: 24 * 60 * 60, label: "6h ago" },
  { secs: Infinity, label: "older" },
];

/// Bucket label for a hit, given the current wall clock.
export function bucketFor(ts: number, now: number): string {
  const age = Math.max(0, now - ts);
  for (const b of BUCKETS) if (age < b.secs) return b.label;
  return BUCKETS[BUCKETS.length - 1].label;
}

/// Insert sticky time-bucket headers between rows. Pinned rows go to
/// the top under a synthetic "pinned" header; everything else flows by
/// timestamp. Input rows are assumed pre-sorted newest-first.
export function withHeaders(
  rows: Row[],
  pinnedHits: ActivityHit[],
  now: number,
): Row[] {
  const out: Row[] = [];
  if (pinnedHits.length > 0) {
    out.push({ kind: "header", key: "h:pinned", label: "pinned" });
    pinnedHits.forEach((h, i) => {
      out.push({
        kind: "hit",
        key: `pin:${hitKey(h, i)}`,
        hit: h,
        pinned: true,
      });
    });
  }
  let last: string | null = null;
  for (const r of rows) {
    const ts =
      r.kind === "hit"
        ? r.hit.ts
        : r.kind === "group"
          ? r.lastTs
          : null;
    if (ts !== null) {
      const label = bucketFor(ts, now);
      if (label !== last) {
        out.push({ kind: "header", key: `h:${label}`, label });
        last = label;
      }
    }
    out.push(r);
  }
  return out;
}

// ── relative-time formatting ────────────────────────────────────────────

/// Cheap relative-time. We avoid date-fns/dayjs for ~30 KB savings —
/// crabcc dashboards run inside `include_str!`-baked HTML and every
/// dependency adds parse-time on the in-memory hot path.
export function relativeTime(ts: number, now: number): string {
  const age = Math.max(0, now - ts);
  if (age < 1) return "now";
  if (age < 60) return `${age}s ago`;
  if (age < 3600) return `${Math.floor(age / 60)}m ago`;
  if (age < 86400) return `${Math.floor(age / 3600)}h ago`;
  return `${Math.floor(age / 86400)}d ago`;
}

// ── virtual-window math ─────────────────────────────────────────────────

export interface VirtualWindow {
  start: number; // first row index to render (inclusive)
  end: number; // last row index to render (exclusive)
  padTop: number; // px spacer above visible slice
  padBottom: number; // px spacer below
}

/// Pure virtualization math — given total rows, scroll offset, viewport
/// height, and row height, return the slice to render plus top/bottom
/// padding. `overscan` rows are rendered above and below the strict
/// viewport so fast scrolls don't expose blank rows for a frame.
export function computeWindow(
  total: number,
  scrollTop: number,
  viewportH: number,
  rowH: number,
  overscan: number,
): VirtualWindow {
  if (total === 0 || rowH <= 0 || viewportH <= 0) {
    return { start: 0, end: 0, padTop: 0, padBottom: 0 };
  }
  const rawStart = Math.floor(scrollTop / rowH) - overscan;
  const visible = Math.ceil(viewportH / rowH) + overscan * 2;
  const start = Math.max(0, rawStart);
  const end = Math.min(total, start + visible);
  return {
    start,
    end,
    padTop: start * rowH,
    padBottom: (total - end) * rowH,
  };
}

// ── pinned-hit storage helpers ──────────────────────────────────────────

/// Stable pin identity — independent of row index so a hit stays pinned
/// even as new rows arrive at the top.
export function pinId(h: ActivityHit): string {
  return `${h.ts}:${h.op}:${h.query}`;
}
