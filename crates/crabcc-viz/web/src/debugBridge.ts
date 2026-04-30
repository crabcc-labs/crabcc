// `window.__crabcc__` — debug bridge surfaced by the live-web app so the
// Chrome MV3 extension (#184) can attach to the dashboard tab and read
// dashboard state directly via `chrome.scripting.executeScript`, without
// going back through `crabcc serve`'s HTTP API.
//
// Schema is intentionally tiny — anything richer should live behind the
// existing `/api/*` routes. The extension subscribes to changes via
// `subscribe()` (returns an unsubscribe function), which avoids polling
// from the extension side.
//
// Versioned (`schemaVersion`) so the extension can refuse to attach
// against an incompatible dashboard build.

export interface CrabccDebugBridge {
  schemaVersion: 1;
  appVersion: string;
  apiBase: string;
  /** Repo path the dashboard is anchored to. */
  repoRoot: string | null;
  /** Last-known number of running agents. */
  agentCount: number;
  /** Last-known activity tail length. */
  activityCount: number;
  /** Snapshot timestamp (Date.now()). */
  updatedAt: number;
  /** Subscribe to state updates. Returns an unsubscribe function. */
  subscribe: (cb: (snapshot: CrabccDebugBridge) => void) => () => void;
}

type Listener = (snap: CrabccDebugBridge) => void;
const listeners = new Set<Listener>();

const initial: Omit<CrabccDebugBridge, "subscribe"> = {
  schemaVersion: 1,
  appVersion: "dev",
  apiBase: "http://localhost:7878",
  repoRoot: null,
  agentCount: 0,
  activityCount: 0,
  updatedAt: Date.now(),
};

const state: CrabccDebugBridge = {
  ...initial,
  subscribe(cb) {
    listeners.add(cb);
    cb(state);
    return () => {
      listeners.delete(cb);
    };
  },
};

export function installDebugBridge(): CrabccDebugBridge {
  (window as unknown as { __crabcc__: CrabccDebugBridge }).__crabcc__ = state;
  // Visible breadcrumb in DevTools — confirms the bridge mounted and lets
  // someone inspecting the page eyeball the schema without grepping source.
  console.log(
    "[crabcc] debug bridge installed at window.__crabcc__ (schema v%d)",
    state.schemaVersion,
    state,
  );
  return state;
}

/** Update the bridge state in-place; notify subscribers. */
export function updateDebugBridge(patch: Partial<Omit<CrabccDebugBridge, "subscribe">>): void {
  Object.assign(state, patch, { updatedAt: Date.now() });
  for (const cb of listeners) {
    try {
      cb(state);
    } catch {
      // listener errors must not break the dashboard
    }
  }
}
