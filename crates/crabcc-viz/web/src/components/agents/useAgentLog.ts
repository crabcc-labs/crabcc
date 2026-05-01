// Streaming tail for one agent's log. Polls /api/agents/{id}/log?since=N
// at a 1.5 s cadence (slightly slower than the prior 1 s panel — the
// log expansion is intentionally less aggressive to avoid hammering the
// server with one in-flight request per visible row).
//
// `enabled` controls whether the polling loop runs at all. When the
// detail card collapses we set it false so the interval stops cold —
// no orphaned timers, no stale state on the next expand.

import { useEffect, useRef, useState } from "react";
import { api } from "../../api";

export interface UseAgentLog {
  body: string;
  loading: boolean;
}

const POLL_MS = 1500;

export function useAgentLog(id: string | null, enabled: boolean): UseAgentLog {
  const [body, setBody] = useState("");
  const [loading, setLoading] = useState(true);
  const cursor = useRef(0);
  const stagnation = useRef(0);

  useEffect(() => {
    if (!enabled || !id) {
      setBody("");
      setLoading(false);
      return;
    }
    let alive = true;
    cursor.current = 0;
    stagnation.current = 0;
    setBody("");
    setLoading(true);

    const tick = async () => {
      try {
        const r = await api.agentLog(id, cursor.current);
        if (!alive) return;
        if (r.body) {
          setBody((prev) => (cursor.current === 0 ? r.body : prev + r.body));
          stagnation.current = 0;
        } else {
          stagnation.current += 1;
        }
        cursor.current = r.cursor || cursor.current;
        setLoading(false);
      } catch {
        // Network errors are non-fatal — try again next tick.
      }
    };
    tick();
    const iv = window.setInterval(() => {
      // Stop polling after 3 ticks with no new bytes — agent is done.
      if (stagnation.current > 3) return;
      tick();
    }, POLL_MS);
    return () => {
      alive = false;
      window.clearInterval(iv);
    };
  }, [id, enabled]);

  return { body, loading };
}
