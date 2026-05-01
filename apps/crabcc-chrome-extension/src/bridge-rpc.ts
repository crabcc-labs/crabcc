// Typed wrappers around `chrome.scripting.executeScript({ world: "MAIN" })`.
// Each wrapper runs a tiny inline function in the page's main world that
// pulls `window.__crabcc__` and invokes the requested method. The function
// is serialized cross-realm, so it CANNOT close over module-level variables
// — every value it needs comes through the `args` array.
//
// All async bridge methods return promises that executeScript awaits for us;
// the result is JSON-serialized back into the extension realm.

import type {
  BridgeMethod,
  BridgeStateSnapshot,
  RpcRequest,
  RpcResponse,
} from "./bridge-types";

/**
 * Run `window.__crabcc__[method](...args)` in the main world of `tabId`.
 * Returns the bridge's return value (deserialized through structured clone).
 *
 * Throws on:
 * - tab missing / not allowed by activeTab
 * - bridge not present on the page (`window.__crabcc__` undefined)
 * - bridge method missing (older bridge schema)
 * - any exception thrown inside the bridge method itself
 */
export async function callBridge<T = unknown>(
  tabId: number,
  method: BridgeMethod,
  args: readonly unknown[],
): Promise<T> {
  // chrome.scripting.executeScript resolves to one InjectionResult per
  // frame. We run on the top frame only (no `allFrames: true`), so there
  // is exactly one entry.
  const results = await chrome.scripting.executeScript({
    target: { tabId },
    world: "MAIN",
    func: invokeBridge,
    args: [method, args as unknown[]],
  });
  const first = results[0];
  if (!first) {
    throw new Error("executeScript returned no frames");
  }
  // Chrome reports per-frame errors via `error`, but the typing predates
  // that field. Cast through `unknown` to read it safely.
  const errored = (first as unknown as { error?: { message?: string } }).error;
  if (errored?.message) {
    throw new Error(errored.message);
  }
  return first.result as T;
}

// Top-level so it's serialized as plain source — Chrome rejects closures
// that reference outer-scope identifiers via TDZ.
function invokeBridge(method: string, args: unknown[]): unknown {
  const bridge = (window as unknown as { __crabcc__?: Record<string, unknown> })
    .__crabcc__;
  if (!bridge) {
    throw new Error(
      "window.__crabcc__ is not present — open a tab running the crabcc dashboard, or any page that has installed the bridge.",
    );
  }
  // Synthetic "schema" / "state" methods — return a serializable snapshot
  // of the bridge state, since the bridge object itself contains functions
  // (which structuredClone refuses).
  if (method === "schema") {
    return (bridge as { schemaVersion?: number }).schemaVersion ?? 0;
  }
  if (method === "state") {
    const out: Record<string, unknown> = {};
    for (const k of Object.keys(bridge)) {
      const v = (bridge as Record<string, unknown>)[k];
      if (typeof v === "function") continue;
      out[k] = v;
    }
    return out;
  }
  const fn = (bridge as Record<string, unknown>)[method];
  if (typeof fn !== "function") {
    throw new Error(`window.__crabcc__.${method} is not a function`);
  }
  return (fn as (...a: unknown[]) => unknown).apply(bridge, args);
}

/**
 * Run a popup-side RPC against the active tab; returns a typed envelope.
 * Caller is responsible for routing capability methods (`captureVisibleTab`
 * etc.) before calling this — `dispatchRpc` only handles bridge methods.
 */
export async function dispatchRpc(
  tabId: number,
  req: RpcRequest,
): Promise<RpcResponse> {
  try {
    const result = await callBridge(tabId, req.method as BridgeMethod, req.args);
    return { id: req.id, ok: true, result };
  } catch (err) {
    return {
      id: req.id,
      ok: false,
      error: err instanceof Error ? err.message : String(err),
    };
  }
}

/** Convenience: pull the bridge's serializable state in one round-trip. */
export function readBridgeState(tabId: number): Promise<BridgeStateSnapshot> {
  return callBridge<BridgeStateSnapshot>(tabId, "state", []);
}
