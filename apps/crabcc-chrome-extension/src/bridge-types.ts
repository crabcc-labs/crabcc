// Public surface of `window.__crabcc__` — kept in sync with
// crates/crabcc-viz/web/src/debugBridge.ts. The two files are duplicated on
// purpose: the dashboard's bundle and the extension's bundle ship to
// different runtimes (page main world vs MV3 service worker), and a shared
// package would force a build dependency between them. The bridge bumps
// `schemaVersion` on every breaking change — read it on connect and refuse
// to attach to anything older than `MIN_SCHEMA`.

export const MIN_SCHEMA = 2;

export type KeyModifier = "Alt" | "Control" | "Meta" | "Shift";

export interface PressKeyOptions {
  selector?: string;
  modifiers?: KeyModifier[];
}

export interface TypeOptions {
  append?: boolean;
  submit?: boolean;
}

export interface AriaSnapshotOptions {
  maxDepth?: number;
  visibleOnly?: boolean;
}

export interface AriaNode {
  ref: string;
  role: string;
  name: string;
  tag: string;
  focusable: boolean;
  visible: boolean;
  x: number;
  y: number;
  width: number;
  height: number;
  children: AriaNode[];
}

export interface CrabccInteractive {
  kind: string;
  text: string;
  x: number;
  y: number;
  width: number;
  height: number;
  id: string;
  class: string;
  query: string;
  link: string;
  visible: boolean;
}

export interface BridgeBuildInfo {
  mode: string;
  builtAt: number;
  gitSha: string;
}

export interface BridgePerfMemory {
  jsHeapSizeLimit: number;
  totalJSHeapSize: number;
  usedJSHeapSize: number;
}

/** Snapshot of the bridge's serializable state — drops methods. */
export interface BridgeStateSnapshot {
  schemaVersion: number;
  appVersion: string;
  apiBase: string;
  repoRoot: string | null;
  agentCount: number;
  activityCount: number;
  telemetryCount: number;
  graphNodeCount: number;
  graphEdgeCount: number;
  sseConnected: boolean;
  sseLastEventAt: number | null;
  renderCount: number;
  buildInfo: BridgeBuildInfo;
  updatedAt: number;
}

/**
 * The (one-way) RPC envelope spoken between popup and service worker.
 * Symmetric request/response shape so the same code can later carry an
 * external transport (WebSocket / native messaging) without changing the
 * popup or the worker.
 *
 * `BridgeMethod`s land in the page's main world via
 * chrome.scripting.executeScript. `CapabilityMethod`s execute in the
 * service-worker realm against Chrome APIs (tabs / scripting / storage),
 * because the page can't reach those APIs.
 */
export interface RpcRequest {
  id: number;
  method: BridgeMethod | CapabilityMethod;
  args: unknown[];
}

export type CapabilityMethod =
  | "captureVisibleTab"
  | "tabInfo"
  | "debuggerAttach"
  | "debuggerDetach"
  | "debuggerEvaluate"
  | "debuggerConsoleList"
  | "debuggerConsoleClear"
  | "debuggerNetworkList"
  | "debuggerNetworkBody"
  | "debuggerNetworkClear";

export interface DebuggerConsoleEntry {
  ts: number;
  /** "log" | "warn" | "error" | "info" | "debug" | "exception" */
  level: string;
  /** Joined argument values, truncated to 4 KB. */
  text: string;
  /** Originating URL, may be empty for inline scripts. */
  source: string;
  line: number;
  column: number;
}

export interface DebuggerNetworkEntry {
  /** When the request fired (Date.now() at requestWillBeSent). */
  ts: number;
  /** CDP request id; pass to `debuggerNetworkBody`. */
  requestId: string;
  url: string;
  method: string;
  /** CDP resource type (Document / XHR / Fetch / Image / …). */
  type: string;
  /** Final HTTP status, or null until response. */
  status: number | null;
  statusText: string;
  mimeType: string | null;
  /** Encoded body size in bytes, null until loadingFinished. */
  size: number | null;
  /** Duration ms, null until loadingFinished. */
  duration: number | null;
  /** True iff the request errored (DNS, abort, blocked, etc.). */
  failed: boolean;
  errorText: string;
}

export interface DebuggerEvaluateResult {
  /** JSON-serialized return value (CDP's RemoteObject.value or .description). */
  value: unknown;
  /** Same shape Chrome returns — "string" / "number" / "object" / etc. */
  type: string;
  /** Iff the expression threw. */
  exception: string | null;
}

export interface DebuggerNetworkBody {
  body: string;
  base64Encoded: boolean;
  /** Empty when no body could be retrieved (e.g. response not yet received). */
  errorText: string;
}

/** Result of a successful captureVisibleTab call. */
export interface CaptureResult {
  /** PNG data URL (`data:image/png;base64,…`). */
  dataUrl: string;
  /** Tab URL captured. */
  url: string;
  /** Date.now() at capture. */
  capturedAt: number;
  /** Approximate byte size of the encoded image. */
  bytes: number;
}

export interface TabInfo {
  id: number | null;
  url: string;
  title: string;
  windowId: number | null;
  status: string;
}

// --- transport (Phase 0.5) ----------------------------------------------

export const DEFAULT_WS_ENDPOINT = "ws://localhost:7878/ws/extension";

export type TransportState = "disconnected" | "connecting" | "connected" | "error";

export interface TransportSnapshot {
  state: TransportState;
  endpoint: string;
  lastError: string | null;
  /** Inbound RpcRequests received since the worker started. */
  rpcsReceived: number;
  /** Date.now() the connection last reached "connected", or 0 if never. */
  connectedAt: number;
}

/** Server → extension hello-style messages (also sent in the reverse). */
export interface TransportHello {
  kind: "hello";
  schema: number;
  version: string;
  /** Method names the extension can dispatch. */
  capabilities: string[];
}

/** App-level keepalive — browser WebSocket can't expose ping/pong frames. */
export interface TransportPing {
  kind: "ping";
  ts: number;
}
export interface TransportPong {
  kind: "pong";
  ts: number;
}

export interface RpcResponse<T = unknown> {
  id: number;
  ok: boolean;
  result?: T;
  error?: string;
}

export type BridgeMethod =
  | "schema"
  | "state"
  | "buttons"
  | "click"
  | "waitFor"
  | "perfMemory"
  | "navigate"
  | "goBack"
  | "goForward"
  | "pressKey"
  | "hover"
  | "type"
  | "selectOption"
  | "drag"
  | "ariaSnapshot"
  | "clickByRef"
  | "hoverByRef"
  | "typeByRef";
