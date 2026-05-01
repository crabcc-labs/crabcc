// Row-pipeline hook — runs filter→group→header on every relevant
// dependency change. Pinned hits are surfaced separately so the
// pinned-block sticks to the top regardless of bucket.
//
// `now` ticks once a second from App.tsx; that re-derives bucket
// labels but does NOT re-derive the filtered/grouped intermediates
// (those memoize on hits + filter / group state).

import { useCallback, useMemo, useState } from "react";
import {
  asHitRows,
  bucketFor,
  groupByOp,
  pinId,
  withHeaders,
} from "./store";
import type { ActivityHit, Row } from "./types";

export interface UseActivityRows {
  rows: Row[];
  groupBy: boolean;
  toggleGroupBy(): void;
  toggleExpanded(key: string): void;
  pinnedIds: Set<string>;
  togglePin(hit: ActivityHit): void;
  unpinAll(): void;
}

export function useActivityRows(
  filtered: ActivityHit[],
  now: number,
): UseActivityRows {
  const [groupBy, setGroupBy] = useState(false);
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  const [pinnedIds, setPinnedIds] = useState<Set<string>>(() => new Set());

  const toggleGroupBy = useCallback(() => setGroupBy((g) => !g), []);
  const toggleExpanded = useCallback((key: string) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }, []);
  const togglePin = useCallback((h: ActivityHit) => {
    const id = pinId(h);
    setPinnedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);
  const unpinAll = useCallback(() => setPinnedIds(new Set()), []);

  const rows = useMemo(() => {
    // Pinned rows are pulled out so they stick at the top under their
    // own header. Use a Map for O(1) ID lookup (rather than includes()).
    const pinnedHits: ActivityHit[] = [];
    const rest: ActivityHit[] = [];
    for (const h of filtered) {
      if (pinnedIds.has(pinId(h))) pinnedHits.push(h);
      else rest.push(h);
    }
    const baseRows = groupBy ? groupByOp(rest, expanded) : asHitRows(rest);
    // bucketFor is referenced indirectly via withHeaders; reference it
    // here so the dependency tracker keeps it in scope (TS otherwise
    // strips the import in some configurations).
    void bucketFor;
    return withHeaders(baseRows, pinnedHits, now);
  }, [filtered, groupBy, expanded, pinnedIds, now]);

  return {
    rows,
    groupBy,
    toggleGroupBy,
    toggleExpanded,
    pinnedIds,
    togglePin,
    unpinAll,
  };
}
