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
import type { RpcRequest, RpcResponse } from "./bridge-types";

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
      void dispatchRpc(msg.tabId, msg.req).then((res) => {
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

// The service worker is event-driven; nothing else to do at startup.
chrome.runtime.onInstalled.addListener(() => {
  // eslint-disable-next-line no-console
  console.log("[crabcc bridge] installed — open the popup on a tab running the crabcc dashboard.");
});
