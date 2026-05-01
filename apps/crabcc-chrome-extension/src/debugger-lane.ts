// Phase 1: chrome.debugger-backed inspection lane. Mirrors the most
// commonly-used DevTools surfaces — console, network, evaluate — without
// pulling in a full CDP client.
//
// Lifecycle: callers explicitly `attach` a tab before any other call.
// Attachment shows Chrome's "extension is debugging" yellow banner — the
// permission cost is real, so we never auto-attach. `detach` releases.
// Tab close auto-detaches via chrome.tabs.onRemoved + chrome.debugger.onDetach.
//
// Buffers: per-tab ring buffers (BUFFER_LIMIT) for console + network
// events. Memory bound is intentional: the worker may be killed before
// the operator reads them, so we trade history-depth for predictability.

import type {
  DebuggerConsoleEntry,
  DebuggerEvaluateResult,
  DebuggerNetworkBody,
  DebuggerNetworkEntry,
  V8HeapSnapshotResult,
  V8MetricEntry,
  V8MetricsResult,
  V8ProfileSummary,
} from "./bridge-types";

const PROTOCOL_VERSION = "1.3";
const BUFFER_LIMIT = 200;

/** Tabs we've attached to. Source-of-truth for whether commands are valid. */
const attached = new Set<number>();

/** Per-tab console entries, oldest first. */
const consoleBufs = new Map<number, DebuggerConsoleEntry[]>();
/** Per-tab network entries, keyed by requestId for in-place updates. */
const networkBufs = new Map<number, Map<string, DebuggerNetworkEntry>>();
/** Tabs with an in-flight Profiler.start. */
const cpuProfiling = new Set<number>();
/** Tabs that have already enabled HeapProfiler / Profiler / Performance. */
const heapProfilerEnabled = new Set<number>();
const profilerEnabled = new Set<number>();
const performanceEnabled = new Set<number>();

let listenersBound = false;

export async function attach(tabId: number): Promise<void> {
  if (attached.has(tabId)) return;
  bindListenersOnce();
  await chrome.debugger.attach({ tabId }, PROTOCOL_VERSION);
  attached.add(tabId);
  // Order matters: enable Runtime first so console events fire before any
  // page script runs after a fresh attach. Network.enable buffers existing
  // in-flight requests too.
  await chrome.debugger.sendCommand({ tabId }, "Runtime.enable", {});
  await chrome.debugger.sendCommand({ tabId }, "Network.enable", {});
}

export async function detach(tabId: number): Promise<void> {
  if (!attached.has(tabId)) return;
  try {
    await chrome.debugger.detach({ tabId });
  } catch {
    // Already detached (tab closed, etc.) — drop cleanly.
  }
  attached.delete(tabId);
  consoleBufs.delete(tabId);
  networkBufs.delete(tabId);
  cpuProfiling.delete(tabId);
  heapProfilerEnabled.delete(tabId);
  profilerEnabled.delete(tabId);
  performanceEnabled.delete(tabId);
}

export function isAttached(tabId: number): boolean {
  return attached.has(tabId);
}

export async function evaluate(
  tabId: number,
  expression: string,
  awaitPromise = false,
): Promise<DebuggerEvaluateResult> {
  ensureAttached(tabId);
  const res = (await chrome.debugger.sendCommand({ tabId }, "Runtime.evaluate", {
    expression,
    returnByValue: true,
    awaitPromise,
    // Allows reading non-publicly-exposed properties (eg. closure vars in
    // some browser builds). Matches DevTools' default eval mode.
    includeCommandLineAPI: true,
  })) as RuntimeEvaluateResult;
  if (res.exceptionDetails) {
    return {
      value: null,
      type: "exception",
      exception: extractExceptionText(res.exceptionDetails),
    };
  }
  return {
    value: res.result?.value ?? res.result?.description ?? null,
    type: res.result?.type ?? "undefined",
    exception: null,
  };
}

export function consoleList(tabId: number, limit?: number): DebuggerConsoleEntry[] {
  const buf = consoleBufs.get(tabId) ?? [];
  if (limit && limit > 0 && buf.length > limit) {
    return buf.slice(buf.length - limit);
  }
  return buf.slice();
}

export function consoleClear(tabId: number): void {
  consoleBufs.delete(tabId);
}

export function networkList(tabId: number, limit?: number): DebuggerNetworkEntry[] {
  const map = networkBufs.get(tabId);
  if (!map) return [];
  // Iteration order is insertion order (Map spec) — gives the operator
  // requests in chronological order without an explicit sort.
  const arr = Array.from(map.values());
  if (limit && limit > 0 && arr.length > limit) {
    return arr.slice(arr.length - limit);
  }
  return arr;
}

export async function networkBody(
  tabId: number,
  requestId: string,
): Promise<DebuggerNetworkBody> {
  ensureAttached(tabId);
  try {
    const res = (await chrome.debugger.sendCommand({ tabId }, "Network.getResponseBody", {
      requestId,
    })) as { body: string; base64Encoded: boolean };
    return { body: res.body, base64Encoded: res.base64Encoded, errorText: "" };
  } catch (err) {
    return {
      body: "",
      base64Encoded: false,
      errorText: err instanceof Error ? err.message : String(err),
    };
  }
}

export function networkClear(tabId: number): void {
  networkBufs.delete(tabId);
}

function ensureAttached(tabId: number): void {
  if (!attached.has(tabId)) {
    throw new Error(`debugger not attached to tab ${tabId} — call debuggerAttach first`);
  }
}

function bindListenersOnce(): void {
  if (listenersBound) return;
  listenersBound = true;

  chrome.debugger.onEvent.addListener((source, method, params) => {
    const tabId = source.tabId;
    if (tabId == null) return;
    if (!attached.has(tabId)) return;
    switch (method) {
      case "Runtime.consoleAPICalled":
        recordConsole(tabId, params as ConsoleApiCalled);
        return;
      case "Runtime.exceptionThrown":
        recordException(tabId, params as ExceptionThrown);
        return;
      case "Network.requestWillBeSent":
        recordRequest(tabId, params as RequestWillBeSent);
        return;
      case "Network.responseReceived":
        updateResponse(tabId, params as ResponseReceived);
        return;
      case "Network.loadingFinished":
        finishLoading(tabId, params as LoadingFinished);
        return;
      case "Network.loadingFailed":
        failLoading(tabId, params as LoadingFailed);
        return;
    }
  });

  chrome.debugger.onDetach.addListener((source) => {
    if (source.tabId == null) return;
    attached.delete(source.tabId);
    consoleBufs.delete(source.tabId);
    networkBufs.delete(source.tabId);
  });

  chrome.tabs.onRemoved.addListener((tabId) => {
    attached.delete(tabId);
    consoleBufs.delete(tabId);
    networkBufs.delete(tabId);
    cpuProfiling.delete(tabId);
    heapProfilerEnabled.delete(tabId);
    profilerEnabled.delete(tabId);
    performanceEnabled.delete(tabId);
  });
}

// --- v8 profiling lane ----------------------------------------------------
//
// All v8 capabilities sit on top of the same chrome.debugger attachment
// the console/network lanes use — they don't take a separate session,
// and the operator pays the same yellow-banner cost once at attach time.
//
// HeapProfiler, Profiler, and Performance each need to be enabled before
// their commands run. We track per-tab "enabled" flags so a second call
// doesn't re-enable redundantly (CDP tolerates it but it's a needless
// round-trip).

async function ensureHeapProfiler(tabId: number): Promise<void> {
  if (heapProfilerEnabled.has(tabId)) return;
  await chrome.debugger.sendCommand({ tabId }, "HeapProfiler.enable", {});
  heapProfilerEnabled.add(tabId);
}

async function ensureProfiler(tabId: number): Promise<void> {
  if (profilerEnabled.has(tabId)) return;
  await chrome.debugger.sendCommand({ tabId }, "Profiler.enable", {});
  profilerEnabled.add(tabId);
}

async function ensurePerformance(tabId: number): Promise<void> {
  if (performanceEnabled.has(tabId)) return;
  await chrome.debugger.sendCommand({ tabId }, "Performance.enable", {});
  performanceEnabled.add(tabId);
}

export async function v8CollectGarbage(tabId: number): Promise<{ collected: true }> {
  ensureAttached(tabId);
  await ensureHeapProfiler(tabId);
  await chrome.debugger.sendCommand({ tabId }, "HeapProfiler.collectGarbage", {});
  return { collected: true };
}

/**
 * Take a heap snapshot. The snapshot is streamed back as many small
 * `HeapProfiler.addHeapSnapshotChunk` events while the `takeHeapSnapshot`
 * command is in flight — we capture them with a one-shot listener and
 * return the concatenated JSON when the command resolves.
 */
export async function v8HeapSnapshot(tabId: number): Promise<V8HeapSnapshotResult> {
  ensureAttached(tabId);
  await ensureHeapProfiler(tabId);
  const chunks: string[] = [];
  const onChunk = (
    source: { tabId?: number },
    method: string,
    params: unknown,
  ): void => {
    if (source.tabId !== tabId) return;
    if (method !== "HeapProfiler.addHeapSnapshotChunk") return;
    const c = (params as { chunk?: string }).chunk;
    if (typeof c === "string") chunks.push(c);
  };
  chrome.debugger.onEvent.addListener(onChunk);
  try {
    await chrome.debugger.sendCommand({ tabId }, "HeapProfiler.takeHeapSnapshot", {
      reportProgress: false,
    });
  } finally {
    chrome.debugger.onEvent.removeListener(onChunk);
  }
  const json = chunks.join("");
  return { json, sizeBytes: json.length, chunkCount: chunks.length };
}

export async function v8ProfileStart(tabId: number): Promise<{ started: true }> {
  ensureAttached(tabId);
  if (cpuProfiling.has(tabId)) {
    throw new Error(`v8.profile.start: profile already running on tab ${tabId}`);
  }
  await ensureProfiler(tabId);
  await chrome.debugger.sendCommand({ tabId }, "Profiler.start", {});
  cpuProfiling.add(tabId);
  return { started: true };
}

export async function v8ProfileStop(tabId: number): Promise<V8ProfileSummary> {
  ensureAttached(tabId);
  if (!cpuProfiling.has(tabId)) {
    throw new Error(`v8.profile.stop: no profile running on tab ${tabId}`);
  }
  const out = (await chrome.debugger.sendCommand({ tabId }, "Profiler.stop", {})) as {
    profile?: ProfilerProfile;
  };
  cpuProfiling.delete(tabId);
  const p = out.profile ?? { nodes: [], samples: [], startTime: 0, endTime: 0 };
  return summariseProfile(p);
}

export async function v8Metrics(tabId: number): Promise<V8MetricsResult> {
  ensureAttached(tabId);
  await ensurePerformance(tabId);
  const out = (await chrome.debugger.sendCommand({ tabId }, "Performance.getMetrics", {})) as {
    metrics?: V8MetricEntry[];
  };
  return { metrics: out.metrics ?? [], ts: Date.now() };
}

interface ProfilerProfile {
  nodes?: { id: number }[];
  samples?: number[];
  startTime?: number;
  endTime?: number;
}

function summariseProfile(p: ProfilerProfile): V8ProfileSummary {
  const nodes = p.nodes ?? [];
  const samples = p.samples ?? [];
  // CDP timestamps are in microseconds — convert to ms.
  const durationMs =
    p.startTime != null && p.endTime != null
      ? Math.max(0, Math.round((p.endTime - p.startTime) / 1000))
      : 0;
  return {
    nodeCount: nodes.length,
    sampleCount: samples.length,
    durationMs,
    profile: p,
  };
}

function pushConsole(tabId: number, entry: DebuggerConsoleEntry): void {
  let buf = consoleBufs.get(tabId);
  if (!buf) {
    buf = [];
    consoleBufs.set(tabId, buf);
  }
  buf.push(entry);
  if (buf.length > BUFFER_LIMIT) buf.splice(0, buf.length - BUFFER_LIMIT);
}

function recordConsole(tabId: number, p: ConsoleApiCalled): void {
  const text = (p.args ?? [])
    .map((a) => formatRemote(a))
    .join(" ")
    .slice(0, 4096);
  const frame = p.stackTrace?.callFrames?.[0];
  pushConsole(tabId, {
    ts: typeof p.timestamp === "number" ? Math.round(p.timestamp) : Date.now(),
    level: p.type ?? "log",
    text,
    source: frame?.url ?? "",
    line: frame?.lineNumber ?? 0,
    column: frame?.columnNumber ?? 0,
  });
}

function recordException(tabId: number, p: ExceptionThrown): void {
  const ed = p.exceptionDetails;
  pushConsole(tabId, {
    ts: typeof p.timestamp === "number" ? Math.round(p.timestamp) : Date.now(),
    level: "exception",
    text: extractExceptionText(ed).slice(0, 4096),
    source: ed?.url ?? "",
    line: ed?.lineNumber ?? 0,
    column: ed?.columnNumber ?? 0,
  });
}

function recordRequest(tabId: number, p: RequestWillBeSent): void {
  let map = networkBufs.get(tabId);
  if (!map) {
    map = new Map();
    networkBufs.set(tabId, map);
  }
  // Cap by evicting the oldest insertion. We can't trim mid-iteration, so
  // do it lazily on insert.
  if (map.size >= BUFFER_LIMIT) {
    const firstKey = map.keys().next().value;
    if (firstKey) map.delete(firstKey);
  }
  map.set(p.requestId, {
    ts: Date.now(),
    requestId: p.requestId,
    url: p.request?.url ?? "",
    method: p.request?.method ?? "GET",
    type: p.type ?? "Other",
    status: null,
    statusText: "",
    mimeType: null,
    size: null,
    duration: null,
    failed: false,
    errorText: "",
  });
}

function updateResponse(tabId: number, p: ResponseReceived): void {
  const entry = networkBufs.get(tabId)?.get(p.requestId);
  if (!entry) return;
  entry.status = p.response?.status ?? null;
  entry.statusText = p.response?.statusText ?? "";
  entry.mimeType = p.response?.mimeType ?? null;
}

function finishLoading(tabId: number, p: LoadingFinished): void {
  const entry = networkBufs.get(tabId)?.get(p.requestId);
  if (!entry) return;
  entry.size = p.encodedDataLength ?? null;
  entry.duration = Date.now() - entry.ts;
}

function failLoading(tabId: number, p: LoadingFailed): void {
  const entry = networkBufs.get(tabId)?.get(p.requestId);
  if (!entry) return;
  entry.failed = true;
  entry.errorText = p.errorText ?? "failed";
  entry.duration = Date.now() - entry.ts;
}

// --- minimal CDP shapes ---------------------------------------------------
//
// We don't depend on @types/chrome-debugger-protocol — it's heavy and we
// only touch a handful of methods. These shapes match the
// devtools-protocol JSON schema for protocol 1.3.

interface RemoteObject {
  type: string;
  subtype?: string;
  value?: unknown;
  description?: string;
}

interface ExceptionDetails {
  text?: string;
  exception?: RemoteObject;
  url?: string;
  lineNumber?: number;
  columnNumber?: number;
}

interface RuntimeEvaluateResult {
  result?: RemoteObject;
  exceptionDetails?: ExceptionDetails;
}

interface ConsoleApiCalled {
  type?: string;
  args?: RemoteObject[];
  timestamp?: number;
  stackTrace?: { callFrames?: { url?: string; lineNumber?: number; columnNumber?: number }[] };
}

interface ExceptionThrown {
  timestamp?: number;
  exceptionDetails?: ExceptionDetails;
}

interface RequestWillBeSent {
  requestId: string;
  request?: { url?: string; method?: string };
  type?: string;
}

interface ResponseReceived {
  requestId: string;
  response?: { status?: number; statusText?: string; mimeType?: string };
}

interface LoadingFinished {
  requestId: string;
  encodedDataLength?: number;
}

interface LoadingFailed {
  requestId: string;
  errorText?: string;
}

function formatRemote(obj: RemoteObject | undefined): string {
  if (!obj) return "";
  if (typeof obj.value === "string") return obj.value;
  if (obj.value !== undefined) {
    try {
      return JSON.stringify(obj.value);
    } catch {
      return String(obj.value);
    }
  }
  return obj.description ?? obj.type;
}

function extractExceptionText(ed: ExceptionDetails | undefined): string {
  if (!ed) return "(no exception details)";
  const parts: string[] = [];
  if (ed.text) parts.push(ed.text);
  if (ed.exception?.description) parts.push(ed.exception.description);
  else if (ed.exception?.value !== undefined) parts.push(formatRemote(ed.exception));
  return parts.join(": ").trim() || "(unknown exception)";
}
