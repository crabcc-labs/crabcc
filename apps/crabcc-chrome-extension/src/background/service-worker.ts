import type { JsonRpcRequest, JsonRpcResponse, PageListPagesResult } from "../types/protocol";
import { RPC_ERROR } from "../types/protocol";

const OFFSCREEN_URL = "offscreen/offscreen.html";

async function ensureOffscreen(): Promise<void> {
  const existing = await chrome.offscreen.hasDocument?.();
  if (existing) return;
  await chrome.offscreen.createDocument({
    url: OFFSCREEN_URL,
    reasons: [chrome.offscreen.Reason.IFRAME_SCRIPTING],
    justification:
      "Holds the long-lived EventSource to crabcc serve; MV3 service workers cannot keep a streaming connection open across idle.",
  });
}

chrome.runtime.onStartup.addListener(() => {
  void ensureOffscreen();
});
chrome.runtime.onInstalled.addListener(() => {
  void ensureOffscreen();
});

chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  if (msg?.kind !== "rpc-request") return false;
  void handle(msg.request as JsonRpcRequest).then(sendResponse);
  return true;
});

async function handle(req: JsonRpcRequest): Promise<JsonRpcResponse> {
  try {
    switch (req.method) {
      case "page.list_pages":
        return { jsonrpc: "2.0", id: req.id, result: await pageListPages() };
      default:
        return {
          jsonrpc: "2.0",
          id: req.id,
          error: { code: RPC_ERROR.METHOD_NOT_FOUND, message: `unknown method: ${req.method}` },
        };
    }
  } catch (e) {
    return {
      jsonrpc: "2.0",
      id: req.id,
      error: { code: RPC_ERROR.INTERNAL, message: String(e) },
    };
  }
}

async function pageListPages(): Promise<PageListPagesResult> {
  const tabs = await chrome.tabs.query({});
  return {
    tabs: tabs
      .filter((t): t is chrome.tabs.Tab & { id: number } => typeof t.id === "number")
      .map((t) => ({
        id: t.id,
        url: t.url ?? "",
        title: t.title ?? "",
        active: t.active === true,
      })),
  };
}
