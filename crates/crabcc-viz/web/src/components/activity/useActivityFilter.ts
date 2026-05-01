// Filter-state hook. Stores `text/op/agent` plus the imperative
// helpers the panel needs (clear / set). Memoizes the filtered slice
// so downstream reducers (group, headers) only re-run when the input
// hits or filter actually change.

import { useCallback, useMemo, useState } from "react";
import { filterHits } from "./store";
import type { ActivityHit, FilterState } from "./types";
import { EMPTY_FILTER } from "./types";

export interface UseActivityFilter {
  filter: FilterState;
  setText(text: string): void;
  setOp(op: string | null): void;
  setAgent(agent: string | null): void;
  clear(): void;
  filtered: ActivityHit[];
}

export function useActivityFilter(items: ActivityHit[]): UseActivityFilter {
  const [filter, setFilter] = useState<FilterState>(EMPTY_FILTER);
  const setText = useCallback(
    (text: string) => setFilter((f) => ({ ...f, text })),
    [],
  );
  const setOp = useCallback(
    (op: string | null) => setFilter((f) => ({ ...f, op })),
    [],
  );
  const setAgent = useCallback(
    (agent: string | null) => setFilter((f) => ({ ...f, agent })),
    [],
  );
  const clear = useCallback(() => setFilter(EMPTY_FILTER), []);
  const filtered = useMemo(() => filterHits(items, filter), [items, filter]);
  return { filter, setText, setOp, setAgent, clear, filtered };
}
