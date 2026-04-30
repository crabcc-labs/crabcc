import { useEffect, useRef, useState } from "react";
import { api } from "../api";

/// Tail of an agent's log file. Polls /api/agents/{id}/log incrementally
/// (?since=cursor) at 1s cadence so a running agent's output streams
/// in. Stops polling when the agent finishes (cursor stops moving for
/// 3 ticks → assume done).
///
/// SSE migration (phase 6 of #17): swap this hook for a single
/// EventSource subscribed to /api/events?topic=agent.{id}.log.
export function AgentLogView({ id }: { id: string }) {
  const [body, setBody] = useState("");
  const cursor = useRef(0);
  const stagnation = useRef(0);

  useEffect(() => {
    let alive = true;
    cursor.current = 0;
    stagnation.current = 0;
    setBody("");

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
      } catch {
        // network errors are non-fatal — try again next tick
      }
    };
    tick();
    const iv = setInterval(() => {
      // Stop polling after 3 ticks with no new bytes — agent is done.
      if (stagnation.current > 3) return;
      tick();
    }, 1000);
    return () => {
      alive = false;
      clearInterval(iv);
    };
  }, [id]);

  return (
    <pre className="agent-log">
      {body || "(loading…)"}
    </pre>
  );
}
