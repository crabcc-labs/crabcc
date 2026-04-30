import { useEffect, useRef, useState } from "react";
import { logFetchErr, logFetchOk } from "./lifecycle";

export type UsePollingOptions<T> = {
  /// Stable label used by the lifecycle logger to dedupe error spells
  /// and shape-change deltas. Pass the API path or panel name. Omit
  /// to skip lifecycle logging entirely (back-compat).
  source?: string;
  /// Reduce a successful response to a one-line summary string.
  /// Subsequent fetches with an equal summary stay silent; a change
  /// emits `<source>: old → new`. Required when `source` is set.
  summarize?: (data: T) => string;
};

/// Generic polling hook — calls `fn` immediately, then on a fixed
/// interval until unmounted. Errors are swallowed (caller can surface
/// via `error`); the hook never re-throws so a flaky server doesn't
/// kill the dashboard.
///
/// `intervalMs` of 0 disables the loop after the initial fetch — useful
/// when an upstream condition (e.g. a kill switch) wants polling
/// suspended without unmounting the consuming component.
///
/// Pass `source` + `summarize` to opt the hook into the
/// once-per-state-transition lifecycle logging (issue #147).
export function usePolling<T>(
  fn: () => Promise<T>,
  intervalMs: number,
  deps: React.DependencyList = [],
  options: UsePollingOptions<T> = {},
): { data: T | null; error: Error | null; refetch: () => void } {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<Error | null>(null);
  const fnRef = useRef(fn);
  fnRef.current = fn;
  const optsRef = useRef(options);
  optsRef.current = options;

  const tick = () => {
    fnRef
      .current()
      .then((v) => {
        setData(v);
        setError(null);
        const { source, summarize } = optsRef.current;
        if (source && summarize) {
          try {
            logFetchOk(source, summarize(v));
          } catch {
            // Logging must never crash the dashboard.
          }
        }
      })
      .catch((e) => {
        const err = e instanceof Error ? e : new Error(String(e));
        setError(err);
        const { source } = optsRef.current;
        if (source) logFetchErr(source, err);
      });
  };

  useEffect(() => {
    tick();
    if (intervalMs <= 0) return;
    const id = setInterval(tick, intervalMs);
    return () => clearInterval(id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [intervalMs, ...deps]);

  return { data, error, refetch: tick };
}
