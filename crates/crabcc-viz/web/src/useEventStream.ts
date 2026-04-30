import { useEffect, useRef, useState } from "react";
import { logSseConnect, logSseDisconnect } from "./lifecycle";

/// Subscribe to a Server-Sent Events stream at `path`. Each line
/// labelled `event: <topic>` followed by `data: <json>` becomes an
/// invocation of the matching handler in `topics`.
///
/// Reconnects with backoff on close — keeps the dashboard alive across
/// crabcc-viz restarts during local development. The `connected` flag
/// drives the live indicator in the header.
export function useEventStream(
  path: string,
  topics: Record<string, (payload: unknown) => void>,
): { connected: boolean } {
  const [connected, setConnected] = useState(false);
  const handlersRef = useRef(topics);
  handlersRef.current = topics;

  useEffect(() => {
    let alive = true;
    let backoff = 500;
    let es: EventSource | null = null;

    const open = () => {
      if (!alive) return;
      es = new EventSource(path);
      es.onopen = () => {
        setConnected(true);
        backoff = 500;
        logSseConnect(path);
      };
      es.onerror = () => {
        setConnected(false);
        logSseDisconnect(path);
        es?.close();
        es = null;
        if (alive) {
          setTimeout(open, backoff);
          backoff = Math.min(backoff * 2, 10_000);
        }
      };
      // Wire one listener per topic the caller cares about.
      for (const topic of Object.keys(handlersRef.current)) {
        es.addEventListener(topic, (ev) => {
          let payload: unknown = ev.data;
          try {
            payload = JSON.parse(ev.data);
          } catch {
            // Non-JSON payload — pass through as a string.
          }
          handlersRef.current[topic]?.(payload);
        });
      }
    };
    open();
    return () => {
      alive = false;
      es?.close();
    };
  }, [path]);

  return { connected };
}
