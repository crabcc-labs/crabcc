import { useEffect, useRef, useState } from "react";

/// Generic polling hook — calls `fn` immediately, then on a fixed
/// interval until unmounted. Errors are swallowed (caller can surface
/// via the optional `onError`); the hook never re-throws so a flaky
/// server doesn't kill the dashboard.
///
/// `intervalMs` of 0 disables the loop after the initial fetch — useful
/// when an upstream condition (e.g. a kill switch) wants polling
/// suspended without unmounting the consuming component.
export function usePolling<T>(
  fn: () => Promise<T>,
  intervalMs: number,
  deps: React.DependencyList = [],
): { data: T | null; error: Error | null; refetch: () => void } {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<Error | null>(null);
  const fnRef = useRef(fn);
  fnRef.current = fn;

  const tick = () => {
    fnRef
      .current()
      .then((v) => {
        setData(v);
        setError(null);
      })
      .catch((e) => setError(e instanceof Error ? e : new Error(String(e))));
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
