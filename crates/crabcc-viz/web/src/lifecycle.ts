// Lifecycle logging — issue #147.
//
// One-line `console.info` per state transition (mount/unmount,
// first-fetch, fetch-error spell, fetch recovery, SSE connect/drop,
// user write action) so devs opening DevTools immediately see what the
// `/live` app is doing without the console drowning under repeating
// poll messages.
//
// The rule is "once per state transition" — repeat polls with the same
// data shape stay silent; only deltas log. Error spells dedupe by
// source so a flaky endpoint emits one line per outage, not one per
// retry.
//
// Set `localStorage.crabcc_silent = "1"` to silence everything.

const LOG_PREFIX = "[crabcc]";

function silenced(): boolean {
  try {
    return globalThis.localStorage?.getItem("crabcc_silent") === "1";
  } catch {
    return false;
  }
}

function info(line: string): void {
  if (silenced()) return;
  console.info(`${LOG_PREFIX} ${line}`);
}

export function logMount(panel: string): void {
  info(`${panel} mounted`);
}

export function logUnmount(panel: string): void {
  info(`${panel} unmounted`);
}

// Track per-source state so repeat polls with unchanged shape stay
// silent and error spells dedupe. Module-level — one map for the
// whole app session.
type SourceState = {
  lastSummary: string | null;
  errorAttempts: number;
};
const state = new Map<string, SourceState>();

function get(source: string): SourceState {
  let s = state.get(source);
  if (!s) {
    s = { lastSummary: null, errorAttempts: 0 };
    state.set(source, s);
  }
  return s;
}

/// First fetch logs the summary; subsequent fetches log only when the
/// summary string changes (caller controls what counts as a "shape
/// change" by what they put in the summary).
export function logFetchOk(source: string, summary: string): void {
  const s = get(source);
  const wasInError = s.errorAttempts > 0;
  if (wasInError) {
    info(`${source} recovered after ${s.errorAttempts} failures`);
    s.errorAttempts = 0;
  }
  if (s.lastSummary === null) {
    info(`${source}: ${summary}`);
  } else if (s.lastSummary !== summary) {
    info(`${source}: ${s.lastSummary} → ${summary}`);
  }
  s.lastSummary = summary;
}

/// Logs once on the first error of a spell. Repeated errors against
/// the same source stay silent until a successful fetch resets the
/// spell.
export function logFetchErr(source: string, err: unknown): void {
  const s = get(source);
  s.errorAttempts += 1;
  if (s.errorAttempts === 1) {
    const msg = err instanceof Error ? err.message : String(err);
    info(`${source} failed: ${msg} (will retry)`);
  }
}

/// Manually mark a source as recovered (rare — usually `logFetchOk`
/// handles this transparently). Useful when the caller knows the
/// retry succeeded out-of-band.
export function logFetchRecovered(source: string): void {
  const s = get(source);
  if (s.errorAttempts > 0) {
    info(`${source} recovered after ${s.errorAttempts} failures`);
    s.errorAttempts = 0;
  }
}

// SSE connect/disconnect — track per-path so the App's single stream
// doesn't double-log if React strict-mode re-mounts.
const sseConnected = new Map<string, boolean>();

export function logSseConnect(path: string): void {
  if (sseConnected.get(path)) return;
  sseConnected.set(path, true);
  info(`SSE connected (${path})`);
}

export function logSseDisconnect(path: string, code?: number): void {
  if (!sseConnected.get(path)) return;
  sseConnected.set(path, false);
  const suffix = code !== undefined ? ` (code=${code})` : "";
  info(`SSE disconnected (${path})${suffix}, reconnecting…`);
}

export function logUserAction(action: string): void {
  info(`${action}`);
}

/// Test-only — clears all per-source state so tests don't leak
/// between cases.
export function __resetLifecycleStateForTests(): void {
  state.clear();
  sseConnected.clear();
}
