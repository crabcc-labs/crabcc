// MV3 service worker — accepts RPC envelopes from the popup, dispatches
// them at the active tab via `chrome.scripting.executeScript({ world:
// "MAIN" })`, and bookkeeps a small per-session counter the popup can
// surface.
//
// No external transport yet (Phase 0). The next phase plugs in a WebSocket
// (talking to crabcc serve) or a native-messaging host without touching
// the popup-side code, because the message format on chrome.runtime
// already mirrors the eventual wire envelope (`RpcRequest` /
// `RpcResponse`).

import { dispatchRpc } from "./bridge-rpc";
import * as dbg from "./debugger-lane";
import * as transport from "./transport";
import type {
  CaptureResult,
  CapabilityMethod,
  DebuggerConsoleEntry,
  DebuggerEvaluateResult,
  DebuggerNetworkBody,
  DebuggerNetworkEntry,
  RpcRequest,
  RpcResponse,
  TabInfo,
  TransportSnapshot,
} from "./bridge-types";

interface SessionStats {
  callsTotal: number;
  callsOk: number;
  callsErr: number;
  lastMethod: string | null;
  lastError: string | null;
  lastAt: number;
}

const stats: SessionStats = {
  callsTotal: 0,
  callsOk: 0,
  callsErr: 0,
  lastMethod: null,
  lastError: null,
  lastAt: 0,
};

type IncomingMessage =
  | { kind: "rpc"; tabId: number; req: RpcRequest }
  | { kind: "stats" }
  | { kind: "transport.snapshot" }
  | { kind: "transport.connect"; endpoint: string }
  | { kind: "transport.disconnect" }
  | { kind: "transport.configure"; endpoint: string; auto: boolean };

type OutgoingMessage =
  | { kind: "rpc"; res: RpcResponse }
  | { kind: "stats"; stats: SessionStats }
  | { kind: "transport.snapshot"; snap: TransportSnapshot }
  | { kind: "ack" };

chrome.runtime.onMessage.addListener(
  (msg: IncomingMessage, _sender, sendResponse: (m: OutgoingMessage) => void) => {
    if (msg.kind === "stats") {
      sendResponse({ kind: "stats", stats });
      return false;
    }
    if (msg.kind === "transport.snapshot") {
      sendResponse({ kind: "transport.snapshot", snap: transport.getSnapshot() });
      return false;
    }
    if (msg.kind === "transport.connect") {
      transport.connect(msg.endpoint);
      sendResponse({ kind: "ack" });
      return false;
    }
    if (msg.kind === "transport.disconnect") {
      transport.disconnect();
      sendResponse({ kind: "ack" });
      return false;
    }
    if (msg.kind === "transport.configure") {
      void transport.configure(msg.endpoint, msg.auto).then(() => {
        sendResponse({ kind: "ack" });
      });
      return true;
    }
    if (msg.kind === "rpc") {
      // Async path — we MUST return `true` synchronously so Chrome keeps
      // the message channel open while we await the bridge result.
      void route(msg.tabId, msg.req).then((res) => {
        stats.callsTotal += 1;
        stats.lastMethod = msg.req.method;
        stats.lastAt = Date.now();
        if (res.ok) {
          stats.callsOk += 1;
          stats.lastError = null;
        } else {
          stats.callsErr += 1;
          stats.lastError = res.error ?? "unknown error";
        }
        sendResponse({ kind: "rpc", res });
      });
      return true;
    }
    return false;
  },
);

const CAPABILITY_METHODS = new Set<CapabilityMethod>([
  "captureVisibleTab",
  "tabInfo",
  "debuggerAttach",
  "debuggerDetach",
  "debuggerEvaluate",
  "debuggerConsoleList",
  "debuggerConsoleClear",
  "debuggerNetworkList",
  "debuggerNetworkBody",
  "debuggerNetworkClear",
  "v8CollectGarbage",
  "v8HeapSnapshot",
  "v8ProfileStart",
  "v8ProfileStop",
  "v8Metrics",
]);

async function route(tabId: number, req: RpcRequest): Promise<RpcResponse> {
  if (CAPABILITY_METHODS.has(req.method as CapabilityMethod)) {
    return runCapability(tabId, req);
  }
  return dispatchRpc(tabId, req as RpcRequest);
}

async function runCapability(tabId: number, req: RpcRequest): Promise<RpcResponse> {
  try {
    let result: unknown;
    switch (req.method as CapabilityMethod) {
      case "captureVisibleTab":
        result = (await captureVisibleTab(tabId)) satisfies CaptureResult;
        break;
      case "tabInfo":
        result = (await readTabInfo(tabId)) satisfies TabInfo;
        break;
      case "debuggerAttach":
        await dbg.attach(tabId);
        result = { attached: true };
        break;
      case "debuggerDetach":
        await dbg.detach(tabId);
        result = { attached: false };
        break;
      case "debuggerEvaluate": {
        const [expression, opts] = req.args as [string, { awaitPromise?: boolean } | undefined];
        if (typeof expression !== "string") {
          return { id: req.id, ok: false, error: "debuggerEvaluate: expression must be a string" };
        }
        result = (await dbg.evaluate(
          tabId,
          expression,
          opts?.awaitPromise ?? false,
        )) satisfies DebuggerEvaluateResult;
        break;
      }
      case "debuggerConsoleList": {
        const [opts] = req.args as [{ limit?: number } | undefined];
        result = dbg.consoleList(tabId, opts?.limit) satisfies DebuggerConsoleEntry[];
        break;
      }
      case "debuggerConsoleClear":
        dbg.consoleClear(tabId);
        result = { cleared: true };
        break;
      case "debuggerNetworkList": {
        const [opts] = req.args as [{ limit?: number } | undefined];
        result = dbg.networkList(tabId, opts?.limit) satisfies DebuggerNetworkEntry[];
        break;
      }
      case "debuggerNetworkBody": {
        const [requestId] = req.args as [string];
        if (typeof requestId !== "string") {
          return { id: req.id, ok: false, error: "debuggerNetworkBody: requestId must be a string" };
        }
        result = (await dbg.networkBody(tabId, requestId)) satisfies DebuggerNetworkBody;
        break;
      }
      case "debuggerNetworkClear":
        dbg.networkClear(tabId);
        result = { cleared: true };
        break;
      case "v8CollectGarbage":
        result = await dbg.v8CollectGarbage(tabId);
        break;
      case "v8HeapSnapshot":
        result = await dbg.v8HeapSnapshot(tabId);
        break;
      case "v8ProfileStart":
        result = await dbg.v8ProfileStart(tabId);
        break;
      case "v8ProfileStop":
        result = await dbg.v8ProfileStop(tabId);
        break;
      case "v8Metrics":
        result = await dbg.v8Metrics(tabId);
        break;
      default:
        return { id: req.id, ok: false, error: `unknown capability ${req.method}` };
    }
    return { id: req.id, ok: true, result };
  } catch (err) {
    return {
      id: req.id,
      ok: false,
      error: err instanceof Error ? err.message : String(err),
    };
  }
}

async function captureVisibleTab(tabId: number): Promise<CaptureResult> {
  const tab = await chrome.tabs.get(tabId);
  // captureVisibleTab is keyed by windowId — passing null means "current".
  // We use the tab's window so the popup can drive captures even when the
  // user has multiple windows open and one isn't focused.
  const windowId = tab.windowId ?? chrome.windows.WINDOW_ID_CURRENT;
  const dataUrl = await chrome.tabs.captureVisibleTab(windowId, { format: "png" });
  // captureVisibleTab returns a data URL; base64 length × 0.75 ≈ raw byte size.
  const b64 = dataUrl.slice(dataUrl.indexOf(",") + 1);
  const bytes = Math.floor((b64.length * 3) / 4);
  return {
    dataUrl,
    url: tab.url ?? "",
    capturedAt: Date.now(),
    bytes,
  };
}

async function readTabInfo(tabId: number): Promise<TabInfo> {
  const tab = await chrome.tabs.get(tabId);
  return {
    id: tab.id ?? null,
    url: tab.url ?? "",
    title: tab.title ?? "",
    windowId: tab.windowId ?? null,
    status: tab.status ?? "",
  };
}

// The service worker is event-driven; nothing else to do at startup.
chrome.runtime.onInstalled.addListener(() => {
  // eslint-disable-next-line no-console
  console.log("[crabcc bridge] installed — open the popup on a tab running the crabcc dashboard.");
});

// Bind the transport's request handler to the same router the popup uses.
// `route()` is closed-over from this file, so external (WS) and internal
// (popup) callers go through identical dispatching, including stats.
transport.setHandler(async (req) => {
  // External RPCs target the active tab in the focused window. A future
  // protocol revision will let the caller pin a specific tab id, but
  // Phase 0.5 keeps the surface narrow.
  const [tab] = await chrome.tabs.query({ active: true, lastFocusedWindow: true });
  if (!tab?.id) {
    return { id: req.id, ok: false, error: "no active tab" };
  }
  const res = await route(tab.id, req);
  // Bookkeep transport-driven calls in the same `stats` block so the
  // popup's counter reflects all activity, not just popup-driven RPCs.
  stats.callsTotal += 1;
  stats.lastMethod = req.method;
  stats.lastAt = Date.now();
  if (res.ok) {
    stats.callsOk += 1;
    stats.lastError = null;
  } else {
    stats.callsErr += 1;
    stats.lastError = res.error ?? "unknown error";
  }
  return res;
});

// Bootstrap on every worker startup (install, browser start, or service-
// worker wake). Auto-connects only when `transport.auto` was previously
// set true via the popup.
void transport.bootstrap();
