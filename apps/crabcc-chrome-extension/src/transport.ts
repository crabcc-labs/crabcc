// Phase 0.5: a single outbound WebSocket from the service worker to a
// configurable endpoint. The remote side (an MCP server, typically
// `crabcc serve`) speaks `RpcRequest` / `RpcResponse` directly — same
// envelope the popup uses, so a request that came over WS and one that
// came from the popup are routed through the same dispatcher.
//
// The browser WebSocket API doesn't expose control-frame ping/pong, so
// liveness is enforced at the app level: the worker emits a `ping` every
// `PING_INTERVAL_MS`, and treats two consecutive missed `pong` replies
// as a dead connection.
//
// Reconnect uses exponential backoff capped at MAX_BACKOFF_MS. Chrome's
// MV3 service worker may be killed mid-backoff; on the next event the
// worker re-bootstraps from chrome.storage and (if `transport.auto` is
// set) reconnects.

import type {
  RpcRequest,
  RpcResponse,
  TransportHello,
  TransportPing,
  TransportPong,
  TransportSnapshot,
  TransportState,
} from "./bridge-types";
import { DEFAULT_WS_ENDPOINT } from "./bridge-types";

const STORAGE_KEYS = {
  endpoint: "transport.endpoint",
  auto: "transport.auto",
  state: "transport.state",
  lastError: "transport.lastError",
  connectedAt: "transport.connectedAt",
} as const;

const PING_INTERVAL_MS = 20_000;
const MAX_MISSED_PONGS = 2;
const MIN_BACKOFF_MS = 500;
const MAX_BACKOFF_MS = 30_000;

type RequestHandler = (req: RpcRequest) => Promise<RpcResponse>;

let socket: WebSocket | null = null;
let state: TransportState = "disconnected";
let lastError: string | null = null;
let endpoint: string = DEFAULT_WS_ENDPOINT;
let rpcsReceived = 0;
let connectedAt = 0;
let backoffMs = MIN_BACKOFF_MS;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
let pingTimer: ReturnType<typeof setInterval> | null = null;
let missedPongs = 0;
let handler: RequestHandler | null = null;
let suppressReconnect = false;

export function setHandler(h: RequestHandler): void {
  handler = h;
}

export function getSnapshot(): TransportSnapshot {
  return { state, endpoint, lastError, rpcsReceived, connectedAt };
}

/** Capabilities advertised in the hello message. Static for Phase 0.5. */
const ADVERTISED_CAPS = [
  "schema",
  "state",
  "buttons",
  "click",
  "waitFor",
  "perfMemory",
  "navigate",
  "goBack",
  "goForward",
  "pressKey",
  "hover",
  "type",
  "selectOption",
  "drag",
  "ariaSnapshot",
  "clickByRef",
  "hoverByRef",
  "typeByRef",
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
];

/**
 * Bootstrap from chrome.storage on worker startup. Reads endpoint + auto
 * flag and (if auto) opens the connection.
 */
export async function bootstrap(): Promise<void> {
  const stored = await chrome.storage.local.get([
    STORAGE_KEYS.endpoint,
    STORAGE_KEYS.auto,
  ]);
  endpoint = (stored[STORAGE_KEYS.endpoint] as string | undefined) ?? DEFAULT_WS_ENDPOINT;
  if (stored[STORAGE_KEYS.auto]) {
    connect(endpoint);
  } else {
    await persistState();
  }
}

export async function configure(ep: string, auto: boolean): Promise<void> {
  endpoint = ep;
  await chrome.storage.local.set({
    [STORAGE_KEYS.endpoint]: ep,
    [STORAGE_KEYS.auto]: auto,
  });
}

export function connect(ep?: string): void {
  if (ep) endpoint = ep;
  cancelReconnect();
  suppressReconnect = false;
  if (socket && (socket.readyState === WebSocket.OPEN || socket.readyState === WebSocket.CONNECTING)) {
    return;
  }
  setState("connecting", null);
  let next: WebSocket;
  try {
    next = new WebSocket(endpoint);
  } catch (err) {
    setState("error", err instanceof Error ? err.message : String(err));
    scheduleReconnect();
    return;
  }
  socket = next;
  next.onopen = () => {
    backoffMs = MIN_BACKOFF_MS;
    missedPongs = 0;
    connectedAt = Date.now();
    setState("connected", null);
    sendHello();
    startPingLoop();
  };
  next.onmessage = (evt) => {
    void onMessage(evt.data);
  };
  next.onclose = () => {
    stopPingLoop();
    socket = null;
    if (suppressReconnect) {
      setState("disconnected", null);
      return;
    }
    setState("disconnected", null);
    scheduleReconnect();
  };
  next.onerror = () => {
    // The WebSocket spec doesn't surface error details for security
    // reasons — any failure shows up as "websocket error".
    setState("error", "websocket error");
  };
}

export function disconnect(): void {
  suppressReconnect = true;
  cancelReconnect();
  stopPingLoop();
  if (socket) {
    try {
      socket.close();
    } catch {
      // ignore — close on a half-open socket can throw
    }
    socket = null;
  }
  setState("disconnected", null);
}

function setState(s: TransportState, err: string | null): void {
  state = s;
  lastError = err;
  void persistState();
}

async function persistState(): Promise<void> {
  await chrome.storage.local.set({
    [STORAGE_KEYS.state]: state,
    [STORAGE_KEYS.lastError]: lastError,
    [STORAGE_KEYS.connectedAt]: connectedAt,
    [STORAGE_KEYS.endpoint]: endpoint,
  });
}

function sendHello(): void {
  const hello: TransportHello = {
    kind: "hello",
    schema: 2,
    version: "0.1.0",
    capabilities: ADVERTISED_CAPS,
  };
  send(hello);
}

function send(payload: unknown): void {
  if (!socket || socket.readyState !== WebSocket.OPEN) return;
  try {
    socket.send(JSON.stringify(payload));
  } catch {
    // The send may race a close; the close handler will scheduleReconnect.
  }
}

async function onMessage(data: unknown): Promise<void> {
  if (typeof data !== "string") return;
  let parsed: unknown;
  try {
    parsed = JSON.parse(data);
  } catch {
    return;
  }
  if (!parsed || typeof parsed !== "object") return;

  const obj = parsed as { kind?: string; id?: unknown; method?: unknown; args?: unknown };

  if (obj.kind === "ping") {
    const pong: TransportPong = { kind: "pong", ts: (parsed as TransportPing).ts };
    send(pong);
    return;
  }
  if (obj.kind === "pong") {
    missedPongs = 0;
    return;
  }
  // RpcRequest shape: { id: number, method: string, args: array }
  if (
    typeof obj.id === "number" &&
    typeof obj.method === "string" &&
    Array.isArray(obj.args)
  ) {
    rpcsReceived++;
    if (!handler) {
      send({ id: obj.id, ok: false, error: "no handler bound" } satisfies RpcResponse);
      return;
    }
    const res = await handler(parsed as RpcRequest);
    send(res);
  }
}

function startPingLoop(): void {
  stopPingLoop();
  pingTimer = setInterval(() => {
    if (!socket || socket.readyState !== WebSocket.OPEN) return;
    if (missedPongs >= MAX_MISSED_PONGS) {
      // Force a close — the onclose handler will reconnect.
      try {
        socket.close();
      } catch {
        // ignore
      }
      return;
    }
    missedPongs++;
    const ping: TransportPing = { kind: "ping", ts: Date.now() };
    send(ping);
  }, PING_INTERVAL_MS);
}

function stopPingLoop(): void {
  if (pingTimer != null) {
    clearInterval(pingTimer);
    pingTimer = null;
  }
}

function scheduleReconnect(): void {
  cancelReconnect();
  reconnectTimer = setTimeout(() => connect(endpoint), backoffMs);
  backoffMs = Math.min(backoffMs * 2, MAX_BACKOFF_MS);
}

function cancelReconnect(): void {
  if (reconnectTimer != null) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
}
