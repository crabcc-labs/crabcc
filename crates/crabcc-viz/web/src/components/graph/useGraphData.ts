// Data hooks — one for the initial seed snapshot, one for on-demand
// expansions / search-by-name. Kept tiny on purpose: every parsing or
// merge concern lives in `store.ts` so this file's job is just I/O.

import { useEffect, useState } from "react";
import type { GraphSnapshot, SeedSnapshot } from "./types";

interface SeedState {
  data: SeedSnapshot | null;
  err: string | null;
  loading: boolean;
}

/**
 * Fetch /api/seed-graph?limit=N. Refetches on `limit` change. Cancels
 * the previous request via a closure flag — the `AbortController`
 * approach would also work, but a flag avoids churning a controller
 * each render and keeps the hook bun-test friendly.
 */
export function useSeedGraph(limit: number): SeedState {
  const [state, setState] = useState<SeedState>({ data: null, err: null, loading: true });
  useEffect(() => {
    let cancelled = false;
    setState((s) => ({ ...s, loading: true, err: null }));
    fetch(`/api/seed-graph?limit=${limit}`)
      .then((r) => (r.ok ? r.json() : Promise.reject(new Error(`${r.status}`))))
      .then((j: SeedSnapshot) => {
        if (!cancelled) setState({ data: j, err: null, loading: false });
      })
      .catch((e: Error) => {
        if (!cancelled) setState({ data: null, err: e.message, loading: false });
      });
    return () => {
      cancelled = true;
    };
  }, [limit]);
  return state;
}

/** Imperative fetch — used by expand-buttons + search-submit. */
export async function fetchExpansion(
  root: string,
  dir: "callers" | "callees",
  depth: number,
): Promise<GraphSnapshot> {
  const r = await fetch(
    `/api/graph?root=${encodeURIComponent(root)}&dir=${dir}&depth=${depth}`,
  );
  if (!r.ok) throw new Error(`${r.status} ${r.statusText}`);
  return (await r.json()) as GraphSnapshot;
}
