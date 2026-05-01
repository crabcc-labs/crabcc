// Compact services snapshot for the KPI tile + the homepage's small
// system-status card. Polls /api/services every 30s — the same cadence
// the full ServicesPanel was using until very recently. We don't share
// the panel's poll because the panel only renders on `#/system` now.

import { useEffect, useState } from "react";
import { api, type DiscoveryReport } from "../../api";

export function useServicesSummary(intervalMs = 30_000): {
  report: DiscoveryReport | null;
  loading: boolean;
} {
  const [report, setReport] = useState<DiscoveryReport | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let alive = true;
    const load = () => {
      api
        .services()
        .then((r) => {
          if (!alive) return;
          setReport(r);
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

  return { report, loading };
}
