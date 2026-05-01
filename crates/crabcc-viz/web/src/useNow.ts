// `useNow` — once-per-second wall-clock subscription via a module-level
// emitter so only the leaves that actually read time re-render each tick.
//
// Why: previously App.tsx held `const [now, setNow] = useState(...)` and
// threaded the value through <ActivityPanel>, <TelemetryPanel>,
// <AgentsPanel>, etc. That made *every* parent (App, RelationsGraph,
// every memoized panel) re-render once a second — a measured idle
// baseline of ~2.4 renders/sec on the dashboard. Most of those renders
// were not consumed: RelationsGraph never reads `now`, but it was
// re-checked every tick because its parent re-ran.
//
// With this hook, the single setInterval lives at module scope. Each
// subscriber (the few leaves that actually format relative ages)
// re-renders, and nothing else does. Idle render rate drops accordingly.

import { useSyncExternalStore } from "react";

let _now = Math.floor(Date.now() / 1000);
const _subs = new Set<() => void>();
let _timer: ReturnType<typeof setInterval> | null = null;

function ensureTimer(): void {
  if (_timer !== null) return;
  _timer = setInterval(() => {
    _now = Math.floor(Date.now() / 1000);
    for (const cb of _subs) cb();
  }, 1000);
}

function subscribe(cb: () => void): () => void {
  ensureTimer();
  _subs.add(cb);
  return () => {
    _subs.delete(cb);
    if (_subs.size === 0 && _timer !== null) {
      clearInterval(_timer);
      _timer = null;
    }
  };
}

function getSnapshot(): number {
  return _now;
}

/**
 * Returns the current wall-clock time in **seconds**, ticking once
 * per second. Components only re-render when the second flips.
 *
 * Use this everywhere a relative age (`12s ago`) is rendered. Don't
 * thread the result through props — call the hook in the leaf.
 */
export function useNow(): number {
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}

/** Test-only: force the cached value (does not notify subscribers). */
export function __setNowForTest(n: number): void {
  _now = n;
}
