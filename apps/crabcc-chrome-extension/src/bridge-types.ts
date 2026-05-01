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

export type CapabilityMethod = "captureVisibleTab" | "tabInfo";

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
