import type { JsonRpcRequest, JsonRpcResponse } from "../types/protocol";

const SSE_URL = "http://localhost:7878/api/chrome-bridge/sse";
const RESPONSE_URL = "http://localhost:7878/api/chrome-bridge/response";

let source: EventSource | undefined;

function connect(): void {
  source?.close();
  source = new EventSource(SSE_URL, { withCredentials: false });

  source.addEventListener("rpc", async (ev: MessageEvent<string>) => {
    let req: JsonRpcRequest;
    try {
      req = JSON.parse(ev.data) as JsonRpcRequest;
    } catch {
      return;
    }
    const res = (await chrome.runtime.sendMessage({ kind: "rpc-request", request: req })) as
      | JsonRpcResponse
      | undefined;
    if (!res) return;
    await fetch(`${RESPONSE_URL}/${encodeURIComponent(String(req.id))}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(res),
    }).catch((e) => console.warn("[offscreen] response POST failed:", e));
  });

  source.addEventListener("error", () => {
    setTimeout(connect, 2_000);
  });
}

connect();
