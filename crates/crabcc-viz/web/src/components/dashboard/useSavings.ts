// Live token-savings poll for the dashboard banner (#648). Reads
// /api/savings (aggregated from ~/.crabcc/usage.log) every 2s so the
// operator can watch the rewrite/read/media/morph hooks reduce tokens
// in real time. Self-contained (mirrors useServicesSummary) so it
// doesn't widen App.tsx's prop plumbing.

import { useEffect, useState } from "react";
import { api, type Savings } from "../../api";

export function useSavings(intervalMs = 2_000): {
  savings: Savings | null;
  loading: boolean;
} {
  const [savings, setSavings] = useState<Savings | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let alive = true;
    const load = () => {
      api
        .savings()
        .then((s) => {
          if (!alive) return;
          setSavings(s);
          setLoading(false);
        })
        .catch(() => {
          if (!alive) return;
          setLoading(false);
        });
    };
    load();
    const t = window.setInterval(load, intervalMs);
    return () => {
      alive = false;
      window.clearInterval(t);
    };
  }, [intervalMs]);

  return { savings, loading };
}
