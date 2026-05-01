// Data hooks for the knowledge view. One fetches the graph snapshot,
// the other fetches a single drawer body on demand. Kept tiny on
// purpose — every parsing concern lives in `store.ts`.

import { useEffect, useState } from "react";
import type { DrawerDetail, KnowledgeSnapshot } from "./types";

interface SnapState {
  data: KnowledgeSnapshot | null;
  err: string | null;
  loading: boolean;
}

/**
 * Fetch /api/memory/graph?limit=N. Refetches on `limit` change. We
 * don't poll — the memory db only changes on explicit user action
 * (`crabcc memory mine …`), so a refetch on user-initiated reload is
 * fine. A manual "refresh" button in the orchestrator can call this
 * via the returned `refetch` thunk.
 */
export function useKnowledgeGraph(limit: number): SnapState & { refetch: () => void } {
  const [state, setState] = useState<SnapState>({ data: null, err: null, loading: true });
  const [tick, setTick] = useState(0);
  useEffect(() => {
    let cancelled = false;
    setState((s) => ({ ...s, loading: true, err: null }));
    fetch(`/api/memory/graph?limit=${limit}`)
      .then((r) => (r.ok ? r.json() : Promise.reject(new Error(`${r.status}`))))
      .then((j: KnowledgeSnapshot) => {
        if (!cancelled) setState({ data: j, err: null, loading: false });
      })
      .catch((e: Error) => {
        if (!cancelled) setState({ data: null, err: e.message, loading: false });
      });
    return () => {
      cancelled = true;
    };
  }, [limit, tick]);
  return { ...state, refetch: () => setTick((t) => t + 1) };
}

/**
 * Imperative fetch for a single drawer body. Returns `null` when the
 * id is empty; the side panel doesn't render anything in that case.
 */
export async function fetchDrawer(id: string): Promise<DrawerDetail | null> {
  if (!id) return null;
  const r = await fetch(`/api/memory/get?id=${encodeURIComponent(id)}`);
  if (!r.ok) throw new Error(`${r.status} ${r.statusText}`);
  return (await r.json()) as DrawerDetail;
}
