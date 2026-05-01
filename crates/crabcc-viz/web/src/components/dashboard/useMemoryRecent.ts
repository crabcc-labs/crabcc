// Tail of `~/.crabcc/.crabcc/memory.db` — the last few drawers, just
// for the home page's "memory" tile. The full graph view lives at
// `#/knowledge`; this is a 30-second poll for the dense overview.

import { useEffect, useState } from "react";
import { logFetchErr, logFetchOk } from "../../lifecycle";

export interface MemoryDrawerSummary {
  id: number;
  wing: string;
  room: string | null;
  source_id: string;
  body_preview: string;
  created_at: number;
}

interface MemoryRecentResponse {
  present: boolean;
  cursor: number;
  drawers: MemoryDrawerSummary[];
}

export function useMemoryRecent(limit = 5, intervalMs = 30_000): {
  drawers: MemoryDrawerSummary[];
  present: boolean;
  loading: boolean;
} {
  const [drawers, setDrawers] = useState<MemoryDrawerSummary[]>([]);
  const [present, setPresent] = useState(false);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let alive = true;
    const load = () => {
      // Memory endpoints aren't in the typed api client yet (they use
      // `additionalProperties: true` in the OpenAPI). Raw fetch is the
      // pragmatic move until that's typed; the response shape is small
      // and stable. Same approach as `KnowledgeView`.
      fetch(`/api/memory/recent?limit=${limit}`)
        .then((r) => (r.ok ? r.json() : Promise.reject(new Error(`${r.status}`))))
        .then((j: MemoryRecentResponse) => {
          if (!alive) return;
          setDrawers(j.drawers ?? []);
          setPresent(Boolean(j.present));
          setLoading(false);
          logFetchOk("/api/memory/recent", `${j.drawers?.length ?? 0} drawers`);
        })
        .catch((e) => {
          if (!alive) return;
          setLoading(false);
          logFetchErr("/api/memory/recent", e);
        });
    };
    load();
    const t = window.setInterval(load, intervalMs);
    return () => {
      alive = false;
      window.clearInterval(t);
    };
  }, [limit, intervalMs]);

  return { drawers, present, loading };
}
