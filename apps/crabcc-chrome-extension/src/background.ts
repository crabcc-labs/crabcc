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
import type {
  CaptureResult,
  CapabilityMethod,
  RpcRequest,
  RpcResponse,
  TabInfo,
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
  | { kind: "stats" };

type OutgoingMessage =
  | { kind: "rpc"; res: RpcResponse }
  | { kind: "stats"; stats: SessionStats };

chrome.runtime.onMessage.addListener(
  (msg: IncomingMessage, _sender, sendResponse: (m: OutgoingMessage) => void) => {
    if (msg.kind === "stats") {
      sendResponse({ kind: "stats", stats });
      return false;
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

const CAPABILITY_METHODS = new Set<CapabilityMethod>(["captureVisibleTab", "tabInfo"]);

async function route(tabId: number, req: RpcRequest): Promise<RpcResponse> {
  if (CAPABILITY_METHODS.has(req.method as CapabilityMethod)) {
    return runCapability(tabId, req);
  }
  return dispatchRpc(tabId, req as RpcRequest);
}

async function runCapability(tabId: number, req: RpcRequest): Promise<RpcResponse> {
  try {
    if (req.method === "captureVisibleTab") {
      const result = await captureVisibleTab(tabId);
      return { id: req.id, ok: true, result };
    }
    if (req.method === "tabInfo") {
      const result = await readTabInfo(tabId);
      return { id: req.id, ok: true, result };
    }
    return { id: req.id, ok: false, error: `unknown capability ${req.method}` };
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
