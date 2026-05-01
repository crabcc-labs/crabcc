// Popup script — wires the buttons in popup.html to RPC calls into the
// service worker. The service worker is the single point that talks to
// `chrome.scripting`, so the popup just round-trips JSON.
//
// We resolve the active tab once on open. activeTab grants the extension
// scripting permission for that tab as long as the popup is alive — close
// the popup, lose the grant.

import type {
  AriaNode,
  BridgeMethod,
  CapabilityMethod,
  CaptureResult,
  RpcRequest,
  RpcResponse,
  TransportSnapshot,
  V8HeapSnapshotResult,
  V8ProfileSummary,
} from "./bridge-types";
import { DEFAULT_WS_ENDPOINT, MIN_SCHEMA } from "./bridge-types";

let nextRpcId = 1;
let activeTabId: number | null = null;

interface SessionStats {
  callsTotal: number;
  callsOk: number;
  callsErr: number;
  lastMethod: string | null;
  lastError: string | null;
  lastAt: number;
}

async function rpc<T = unknown>(
  method: BridgeMethod | CapabilityMethod,
  args: unknown[],
): Promise<RpcResponse<T>> {
  if (activeTabId == null) {
    return { id: -1, ok: false, error: "no active tab" };
  }
  const req: RpcRequest = { id: nextRpcId++, method, args };
  const reply = (await chrome.runtime.sendMessage({ kind: "rpc", tabId: activeTabId, req })) as
    | { kind: "rpc"; res: RpcResponse<T> }
    | undefined;
  if (!reply) return { id: req.id, ok: false, error: "service worker did not respond" };
  return reply.res;
}

async function fetchStats(): Promise<SessionStats | null> {
  const reply = (await chrome.runtime.sendMessage({ kind: "stats" })) as
    | { kind: "stats"; stats: SessionStats }
    | undefined;
  return reply?.stats ?? null;
}

async function fetchTransport(): Promise<TransportSnapshot | null> {
  const reply = (await chrome.runtime.sendMessage({ kind: "transport.snapshot" })) as
    | { kind: "transport.snapshot"; snap: TransportSnapshot }
    | undefined;
  return reply?.snap ?? null;
}

function el<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) throw new Error(`#${id} missing from popup.html`);
  return node as T;
}

function setText(id: string, text: string, klass?: "err" | "ok" | "muted"): void {
  const node = el<HTMLElement>(id);
  node.textContent = text;
  node.classList.remove("err", "ok", "muted");
  if (klass) node.classList.add(klass);
}

function showResult(label: string, value: unknown, ok: boolean): void {
  const node = el<HTMLPreElement>("result");
  let body: string;
  try {
    body = typeof value === "string" ? value : JSON.stringify(value, null, 2);
  } catch {
    body = String(value);
  }
  // Truncate huge bodies (ARIA snapshots can be big) so the popup stays
  // responsive — full payload still appears in the worker's logs.
  if (body.length > 4000) body = body.slice(0, 4000) + "\n…(truncated)";
  node.textContent = `${ok ? "OK" : "ERR"} · ${label}\n\n${body}`;
  node.classList.toggle("err", !ok);
  node.classList.toggle("ok", ok);
}

function summariseAria(node: AriaNode): string {
  let total = 0;
  let visible = 0;
  let focusable = 0;
  function walk(n: AriaNode): void {
    total++;
    if (n.visible) visible++;
    if (n.focusable) focusable++;
    for (const c of n.children) walk(c);
  }
  walk(node);
  return `${total} nodes · ${visible} visible · ${focusable} focusable`;
}

async function refreshTab(): Promise<void> {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  activeTabId = tab?.id ?? null;
  setText("tab-url", tab?.url ?? "—", tab?.url ? undefined : "muted");
  if (activeTabId == null) return;
  // Reading the schema doubles as a connect probe. Failure → bridge isn't
  // installed on the page (or activeTab isn't granted yet).
  const schemaRes = await rpc<number>("schema", []);
  if (!schemaRes.ok) {
    setText("schema", "schema —", "err");
    showResult("connect", schemaRes.error ?? "unknown error", false);
    return;
  }
  const v = schemaRes.result ?? 0;
  if (v < MIN_SCHEMA) {
    setText("schema", `schema v${v} (need v${MIN_SCHEMA})`, "err");
  } else {
    setText("schema", `schema v${v}`, "ok");
  }
}

async function refreshStats(): Promise<void> {
  const s = await fetchStats();
  if (!s) return;
  setText("stats", `${s.callsOk} ok · ${s.callsErr} err`);
  setText(
    "last",
    s.lastMethod
      ? `${s.lastMethod}${s.lastError ? ` · ${s.lastError}` : ""}`
      : "—",
    s.lastError ? "err" : s.lastMethod ? "ok" : "muted",
  );
}

async function runMethod(button: HTMLButtonElement): Promise<void> {
  const method = button.dataset.method as
    | BridgeMethod
    | CapabilityMethod
    | undefined;
  if (!method) return;
  const arity = button.dataset.arity ?? "";
  let args: unknown[] = [];
  let label: string = method;

  if (method === "navigate") {
    const url = el<HTMLInputElement>("nav-url").value.trim();
    if (!url) return showResult("navigate", "URL is empty", false);
    args = [url];
    label = `navigate ${url}`;
  } else if (method === "pressKey") {
    const key = el<HTMLInputElement>("key").value;
    if (!key) return showResult("pressKey", "key is empty", false);
    args = [key];
    label = `pressKey ${key}`;
  } else if (arity === "sel") {
    const sel = el<HTMLInputElement>("sel").value.trim();
    if (!sel) return showResult(method, "selector is empty", false);
    args = [sel];
    label = `${method} ${sel}`;
  } else if (arity === "sel-text") {
    const sel = el<HTMLInputElement>("sel").value.trim();
    const text = el<HTMLInputElement>("type-text").value;
    if (!sel) return showResult(method, "selector is empty", false);
    if (method === "type") {
      const submit = el<HTMLInputElement>("type-submit").checked;
      args = [sel, text, { submit }];
      label = `type "${text}" → ${sel}`;
    } else {
      // selectOption
      args = [sel, text];
      label = `selectOption ${sel} = ${text}`;
    }
  } else if (method === "ariaSnapshot") {
    args = [{ visibleOnly: false }];
  } else if (method === "debuggerEvaluate") {
    const expr = el<HTMLInputElement>("eval-expr").value;
    if (!expr) return showResult(method, "expression is empty", false);
    args = [expr, { awaitPromise: true }];
    label = `eval ${expr}`;
  } else if (method === "debuggerConsoleList" || method === "debuggerNetworkList") {
    args = [{ limit: 50 }];
  }

  const res = await rpc(method, args);
  if (method === "ariaSnapshot" && res.ok) {
    const node = res.result as AriaNode;
    setText("snap-summary", summariseAria(node));
  }
  if (method === "captureVisibleTab" && res.ok) {
    const cap = res.result as CaptureResult;
    showCapture(cap);
    // Replace the result pane with a compact summary; the full data URL
    // would balloon the pre tag past the 4 KB cap and clobber readability.
    showResult(
      label,
      `${(cap.bytes / 1024).toFixed(1)} KB · ${cap.url}`,
      true,
    );
    void refreshStats();
    return;
  }
  if (method === "v8HeapSnapshot" && res.ok) {
    const snap = res.result as V8HeapSnapshotResult;
    showHeapSnapshot(snap);
    showResult(
      label,
      `${(snap.sizeBytes / 1024 / 1024).toFixed(2)} MB · ${snap.chunkCount} chunks`,
      true,
    );
    void refreshStats();
    return;
  }
  if (method === "v8ProfileStop" && res.ok) {
    const sum = res.result as V8ProfileSummary;
    showCpuProfile(sum);
    showResult(
      label,
      `${sum.sampleCount} samples · ${sum.nodeCount} nodes · ${sum.durationMs} ms`,
      true,
    );
    void refreshStats();
    return;
  }
  showResult(label, res.ok ? res.result : res.error, res.ok);
  void refreshStats();
}

function blobDownload(
  anchorId: string,
  filename: string,
  payload: string,
  mime: string,
): void {
  const blob = new Blob([payload], { type: mime });
  const url = URL.createObjectURL(blob);
  const dl = el<HTMLAnchorElement>(anchorId);
  dl.href = url;
  dl.download = filename;
  dl.textContent = `download ${filename} (${(payload.length / 1024 / 1024).toFixed(2)} MB)`;
  dl.hidden = false;
}

function showHeapSnapshot(snap: V8HeapSnapshotResult): void {
  const stamp = new Date()
    .toISOString()
    .replace(/[:.]/g, "-")
    .slice(0, 19);
  blobDownload("heap-dl", `crabcc-${stamp}.heapsnapshot`, snap.json, "application/json");
}

function showCpuProfile(sum: V8ProfileSummary): void {
  const stamp = new Date()
    .toISOString()
    .replace(/[:.]/g, "-")
    .slice(0, 19);
  // .cpuprofile files are JSON; serializing with no formatting keeps the
  // download small. DevTools loads them via "Load profile…" in the
  // Performance / Memory panel.
  blobDownload(
    "cpu-dl",
    `crabcc-${stamp}.cpuprofile`,
    JSON.stringify(sum.profile),
    "application/json",
  );
}

function showCapture(cap: CaptureResult): void {
  const img = el<HTMLImageElement>("thumb");
  img.src = cap.dataUrl;
  img.hidden = false;
  const dl = el<HTMLAnchorElement>("dl");
  const stamp = new Date(cap.capturedAt)
    .toISOString()
    .replace(/[:.]/g, "-")
    .slice(0, 19);
  dl.href = cap.dataUrl;
  dl.download = `crabcc-${stamp}.png`;
  dl.textContent = `download (${(cap.bytes / 1024).toFixed(1)} KB)`;
  dl.hidden = false;
}

function bind(): void {
  for (const btn of Array.from(document.querySelectorAll<HTMLButtonElement>("button[data-method]"))) {
    btn.addEventListener("click", () => {
      void runMethod(btn);
    });
  }
  el<HTMLButtonElement>("ws-connect").addEventListener("click", () => {
    void onConnect();
  });
  el<HTMLButtonElement>("ws-disconnect").addEventListener("click", () => {
    void onDisconnect();
  });
  el<HTMLInputElement>("ws-auto").addEventListener("change", () => {
    // Save the auto-flag along with the current endpoint so they're
    // applied as a unit on the next worker bootstrap.
    void chrome.runtime.sendMessage({
      kind: "transport.configure",
      endpoint: currentEndpoint(),
      auto: el<HTMLInputElement>("ws-auto").checked,
    });
  });
}

function currentEndpoint(): string {
  return el<HTMLInputElement>("ws-endpoint").value.trim() || DEFAULT_WS_ENDPOINT;
}

async function onConnect(): Promise<void> {
  const ep = currentEndpoint();
  await chrome.runtime.sendMessage({
    kind: "transport.configure",
    endpoint: ep,
    auto: el<HTMLInputElement>("ws-auto").checked,
  });
  await chrome.runtime.sendMessage({ kind: "transport.connect", endpoint: ep });
  void refreshTransport();
}

async function onDisconnect(): Promise<void> {
  await chrome.runtime.sendMessage({ kind: "transport.disconnect" });
  // Also clear the auto-flag — the operator just told us to stop.
  el<HTMLInputElement>("ws-auto").checked = false;
  await chrome.runtime.sendMessage({
    kind: "transport.configure",
    endpoint: currentEndpoint(),
    auto: false,
  });
  void refreshTransport();
}

async function refreshTransport(): Promise<void> {
  const snap = await fetchTransport();
  if (!snap) return;
  const epInput = el<HTMLInputElement>("ws-endpoint");
  if (!epInput.value) epInput.value = snap.endpoint;
  const klass: "ok" | "err" | "muted" =
    snap.state === "connected" ? "ok" : snap.state === "error" ? "err" : "muted";
  setText("ws-state", snap.lastError ? `${snap.state} · ${snap.lastError}` : snap.state, klass);
  setText("ws-stats", `${snap.rpcsReceived} rpcs received`, "muted");
}

document.addEventListener("DOMContentLoaded", () => {
  bind();
  void refreshTab();
  void refreshStats();
  void refreshTransport();
  // Storage-as-broadcast: the worker writes transport state into
  // chrome.storage on every transition, so the popup gets live updates
  // without polling. Limit refreshes to relevant keys to avoid noise.
  chrome.storage.onChanged.addListener((changes, area) => {
    if (area !== "local") return;
    if (
      "transport.state" in changes ||
      "transport.lastError" in changes ||
      "transport.endpoint" in changes
    ) {
      void refreshTransport();
    }
  });
});
