// Popup script — wires the buttons in popup.html to RPC calls into the
// service worker. The service worker is the single point that talks to
// `chrome.scripting`, so the popup just round-trips JSON.
//
// We resolve the active tab once on open. activeTab grants the extension
// scripting permission for that tab as long as the popup is alive — close
// the popup, lose the grant.

import type { RpcRequest, RpcResponse, BridgeMethod, AriaNode } from "./bridge-types";
import { MIN_SCHEMA } from "./bridge-types";

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

async function rpc<T = unknown>(method: BridgeMethod, args: unknown[]): Promise<RpcResponse<T>> {
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
  const method = button.dataset.method as BridgeMethod | undefined;
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
  }

  const res = await rpc(method, args);
  if (method === "ariaSnapshot" && res.ok) {
    const node = res.result as AriaNode;
    setText("snap-summary", summariseAria(node));
  }
  showResult(label, res.ok ? res.result : res.error, res.ok);
  void refreshStats();
}

function bind(): void {
  for (const btn of Array.from(document.querySelectorAll<HTMLButtonElement>("button[data-method]"))) {
    btn.addEventListener("click", () => {
      void runMethod(btn);
    });
  }
}

document.addEventListener("DOMContentLoaded", () => {
  bind();
  void refreshTab();
  void refreshStats();
});
