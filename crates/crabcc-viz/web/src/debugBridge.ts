// `window.__crabcc__` — debug bridge surfaced by the live-web app so the
// Chrome MV3 extension (#184) and external automation can attach to the
// dashboard tab and read state / drive actions directly via
// `chrome.scripting.executeScript`, without going back through
// `crabcc serve`'s HTTP API.
//
// Schema is intentionally tiny — anything richer should live behind the
// existing `/api/*` routes. The extension subscribes to changes via
// `subscribe()` (returns an unsubscribe function), which avoids polling
// from the extension side.
//
// Versioned (`schemaVersion`) so the extension can refuse to attach
// against an incompatible dashboard build. New fields are *additive* —
// readers must tolerate missing fields and never use `Object.keys()`
// to discover the schema.

export interface CrabccDebugBridge {
  schemaVersion: 2;
  appVersion: string;
  apiBase: string;
  /** Repo path the dashboard is anchored to. */
  repoRoot: string | null;
  /** Last-known number of running agents. */
  agentCount: number;
  /** Last-known activity tail length. */
  activityCount: number;
  /** Last-known telemetry tail length. */
  telemetryCount: number;
  /** Relations-graph node count (0 until the graph component mounts). */
  graphNodeCount: number;
  /** Relations-graph edge count. */
  graphEdgeCount: number;
  /** Whether the SSE event stream is currently connected. */
  sseConnected: boolean;
  /** Date.now() of the last SSE message. `null` until the first event. */
  sseLastEventAt: number | null;
  /** React renders since mount — increments cheaply, useful for sanity checks. */
  renderCount: number;
  /** Ring buffer of recent uncaught errors (last 16). */
  errors: BridgeError[];
  /** Ring buffer of recent app log lines from `lifecycle.ts` (last 64). */
  logs: BridgeLog[];
  /** Build metadata baked at bundle time. */
  buildInfo: BridgeBuildInfo;
  /** Snapshot timestamp (Date.now()). */
  updatedAt: number;
  /** Subscribe to state updates. Returns an unsubscribe function. */
  subscribe: (cb: (snapshot: CrabccDebugBridge) => void) => () => void;
  /**
   * Snapshot every interactive element in the current document
   * (buttons, links, role="button", inputs[type=submit]) with enough
   * identifying info for callers to click them remotely via
   * `chrome.scripting.executeScript({ func, args })`.
   */
  buttons: () => CrabccInteractive[];
  /**
   * Click an element by CSS selector. Returns true if a visible element
   * was found and `.click()` invoked. Safer than asking the extension to
   * synthesize a real mouse event because it short-circuits when the
   * element isn't currently in the DOM.
   */
  click: (selector: string) => boolean;
  /**
   * Resolve once an element matching `selector` appears (or rejects on
   * timeout). Lets the extension wait for late-rendered React subtrees
   * without race-y polling logic on its side.
   */
  waitFor: (selector: string, timeoutMs?: number) => Promise<CrabccInteractive | null>;
  /** Best-effort `performance.memory` snapshot (Chrome-only). */
  perfMemory: () => BridgePerfMemory | null;
  /** Set `location.href`. Returns true unless navigation throws (sandboxed iframes). */
  navigate: (url: string) => boolean;
  /** `history.back()`. */
  goBack: () => void;
  /** `history.forward()`. */
  goForward: () => void;
  /**
   * Synthesize a keydown/keypress/keyup sequence. `target` defaults to
   * `document.activeElement` (or document if none). Single-char keys also
   * dispatch a `beforeinput`/`input` pair on contenteditable / inputs so
   * React onChange handlers fire.
   */
  pressKey: (key: string, opts?: PressKeyOptions) => boolean;
  /**
   * Synthesize a mouseenter/mouseover/mousemove sequence over `selector`.
   * Returns true if the element was found and visible.
   */
  hover: (selector: string) => boolean;
  /**
   * Set the value of an `<input>`/`<textarea>`/contenteditable matching
   * `selector` and dispatch input + change events so React listeners fire.
   * Pass `submit: true` to additionally synthesize Enter.
   */
  type: (selector: string, text: string, opts?: TypeOptions) => boolean;
  /** Set `<select>` value (by option value or visible text) + dispatch change. */
  selectOption: (selector: string, value: string) => boolean;
  /**
   * Synthesize an HTML5 drag from `startSelector` to `endSelector`. Note:
   * synthetic drag events are ignored by some libraries that read native
   * `DataTransfer`; this is best-effort.
   */
  drag: (startSelector: string, endSelector: string) => boolean;
  /**
   * Walk the DOM and return a tree of accessible nodes (role + name +
   * synthetic `ref` id). The `ref` is stable for the lifetime of the
   * snapshot only — pass it to `clickByRef` / `hoverByRef` etc.
   */
  ariaSnapshot: (opts?: AriaSnapshotOptions) => AriaNode;
  /** Click the element captured under `ref` by the most recent ariaSnapshot. */
  clickByRef: (ref: string) => boolean;
  /** Hover the element captured under `ref`. */
  hoverByRef: (ref: string) => boolean;
  /** Type into the element captured under `ref`. */
  typeByRef: (ref: string, text: string, opts?: TypeOptions) => boolean;
}

export interface PressKeyOptions {
  /** CSS selector for the target. Defaults to `document.activeElement`. */
  selector?: string;
  /** Hold these modifiers during the synthesized event. */
  modifiers?: KeyModifier[];
}

export type KeyModifier = "Alt" | "Control" | "Meta" | "Shift";

export interface TypeOptions {
  /** Append (true) vs replace (false, default). */
  append?: boolean;
  /** Synthesize Enter after typing. */
  submit?: boolean;
}

export interface AriaSnapshotOptions {
  /** Limit walk depth (default unlimited). */
  maxDepth?: number;
  /** Skip nodes outside the viewport. */
  visibleOnly?: boolean;
}

export interface AriaNode {
  ref: string;
  role: string;
  name: string;
  /** Tag name lowercased (e.g. "button", "a"). */
  tag: string;
  /** Truthy iff the node is focusable (href / tabindex / form control). */
  focusable: boolean;
  /** Truthy iff the bounding rect is non-zero. */
  visible: boolean;
  /** Page-relative bounds. */
  x: number;
  y: number;
  width: number;
  height: number;
  /** Children in document order. */
  children: AriaNode[];
}

export interface CrabccInteractive {
  /** Element kind: "button" | "link" | "submit" | "role-button". */
  kind: string;
  /** Visible text (trimmed, max 80 chars). */
  text: string;
  /** Page-relative position of the element's top-left corner. */
  x: number;
  y: number;
  width: number;
  height: number;
  /** `id` attribute, or empty string. */
  id: string;
  /** `className` (space-separated), or empty string. */
  class: string;
  /**
   * Generated CSS selector that uniquely identifies the element from
   * `document` — uses `#id` when present, else a tag/class/nth-of-type
   * path. Stable enough for `chrome.scripting.executeScript` to call
   * `document.querySelector(query).click()` without ambiguity.
   */
  query: string;
  /** `href` for links, otherwise empty. */
  link: string;
  /** Whether the element is currently visible (rect non-zero). */
  visible: boolean;
}

export interface BridgeError {
  message: string;
  stack: string;
  /** "error" | "unhandledrejection" | "manual". */
  source: string;
  /** Date.now() when caught. */
  ts: number;
}

export interface BridgeLog {
  level: string;
  message: string;
  ts: number;
}

export interface BridgeBuildInfo {
  /** Build profile: "production" | "development". */
  mode: string;
  /** Date.now() at bundle time (filled at build via define). */
  builtAt: number;
  /** Git SHA short, or empty if not embedded. */
  gitSha: string;
}

export interface BridgePerfMemory {
  jsHeapSizeLimit: number;
  totalJSHeapSize: number;
  usedJSHeapSize: number;
}

type Listener = (snap: CrabccDebugBridge) => void;
const listeners = new Set<Listener>();
const MAX_LISTENERS = 32;
const ERR_RING = 16;
const LOG_RING = 64;

type BridgeMethods =
  | "subscribe"
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

const initial: Omit<CrabccDebugBridge, BridgeMethods> = {
  schemaVersion: 2,
  appVersion: "dev",
  apiBase: "http://localhost:7878",
  repoRoot: null,
  agentCount: 0,
  activityCount: 0,
  telemetryCount: 0,
  graphNodeCount: 0,
  graphEdgeCount: 0,
  sseConnected: false,
  sseLastEventAt: null,
  renderCount: 0,
  errors: [],
  logs: [],
  buildInfo: readBuildInfo(),
  updatedAt: Date.now(),
};

const state: CrabccDebugBridge = {
  ...initial,
  subscribe(cb) {
    if (listeners.size >= MAX_LISTENERS) {
      // Reject silently — the extension shouldn't be able to leak
      // listeners here. Loud throw would break callers that legitimately
      // re-subscribe across hot reloads.
      return () => {};
    }
    listeners.add(cb);
    try {
      cb(state);
    } catch {
      // Initial fire must not break subscribe contract.
    }
    return () => {
      listeners.delete(cb);
    };
  },
  buttons: snapshotInteractives,
  click: clickBySelector,
  waitFor: waitForSelector,
  perfMemory: readPerfMemory,
  navigate: navigateTo,
  goBack: () => {
    if (typeof history !== "undefined") history.back();
  },
  goForward: () => {
    if (typeof history !== "undefined") history.forward();
  },
  pressKey: pressKey,
  hover: hoverBySelector,
  type: typeIntoSelector,
  selectOption: selectOptionBySelector,
  drag: dragBetweenSelectors,
  ariaSnapshot: takeAriaSnapshot,
  clickByRef: (ref) => {
    const el = resolveRef(ref);
    if (!el) return false;
    try {
      el.click();
      return true;
    } catch {
      return false;
    }
  },
  hoverByRef: (ref) => {
    const el = resolveRef(ref);
    return el ? hoverElement(el) : false;
  },
  typeByRef: (ref, text, opts) => {
    const el = resolveRef(ref);
    return el ? typeIntoElement(el, text, opts) : false;
  },
};

const INTERACTIVE_SELECTOR = [
  "button",
  "a[href]",
  '[role="button"]',
  'input[type="submit"]',
  'input[type="button"]',
].join(",");

export function snapshotInteractives(): CrabccInteractive[] {
  if (typeof document === "undefined") return [];
  const out: CrabccInteractive[] = [];
  let els: NodeListOf<HTMLElement>;
  try {
    els = document.querySelectorAll<HTMLElement>(INTERACTIVE_SELECTOR);
  } catch {
    return [];
  }
  els.forEach((el) => {
    let rect: DOMRect;
    try {
      rect = el.getBoundingClientRect();
    } catch {
      return;
    }
    const kind =
      el.tagName === "A"
        ? "link"
        : el.tagName === "INPUT"
          ? (el as HTMLInputElement).type === "submit"
            ? "submit"
            : "button"
          : el.tagName === "BUTTON"
            ? "button"
            : "role-button";
    const text = (el.textContent || (el as HTMLInputElement).value || "")
      .replace(/\s+/g, " ")
      .trim()
      .slice(0, 80);
    out.push({
      kind,
      text,
      x: Math.round(rect.left + window.scrollX),
      y: Math.round(rect.top + window.scrollY),
      width: Math.round(rect.width),
      height: Math.round(rect.height),
      id: el.id || "",
      class: el.className || "",
      query: cssSelectorFor(el),
      link: el.tagName === "A" ? (el as HTMLAnchorElement).href : "",
      visible: rect.width > 0 && rect.height > 0,
    });
  });
  return out;
}

function clickBySelector(selector: string): boolean {
  if (typeof document === "undefined") return false;
  let el: HTMLElement | null = null;
  try {
    el = document.querySelector<HTMLElement>(selector);
  } catch {
    return false;
  }
  if (!el) return false;
  const r = el.getBoundingClientRect();
  if (r.width === 0 || r.height === 0) return false;
  try {
    el.click();
    return true;
  } catch {
    return false;
  }
}

function waitForSelector(
  selector: string,
  timeoutMs = 5000,
): Promise<CrabccInteractive | null> {
  return new Promise((resolve) => {
    if (typeof document === "undefined") return resolve(null);
    const startedAt = Date.now();
    const tick = () => {
      let el: HTMLElement | null = null;
      try {
        el = document.querySelector<HTMLElement>(selector);
      } catch {
        return resolve(null);
      }
      if (el) {
        const all = snapshotInteractives();
        const match = all.find((b) => b.query === cssSelectorFor(el!)) ?? null;
        return resolve(match);
      }
      if (Date.now() - startedAt > timeoutMs) return resolve(null);
      setTimeout(tick, 50);
    };
    tick();
  });
}

function readPerfMemory(): BridgePerfMemory | null {
  const perf = (typeof performance !== "undefined"
    ? (performance as unknown as { memory?: BridgePerfMemory })
    : null);
  if (!perf?.memory) return null;
  const m = perf.memory;
  return {
    jsHeapSizeLimit: m.jsHeapSizeLimit,
    totalJSHeapSize: m.totalJSHeapSize,
    usedJSHeapSize: m.usedJSHeapSize,
  };
}

function readBuildInfo(): BridgeBuildInfo {
  // esbuild's `define:` substitutes the right-hand side at bundle time.
  // We read via a guarded global so test environments (no define) don't
  // crash; falls back to "dev" + 0 + "".
  const g = globalThis as unknown as {
    __CRABCC_BUILD_MODE__?: string;
    __CRABCC_BUILD_TS__?: number;
    __CRABCC_GIT_SHA__?: string;
  };
  return {
    mode: g.__CRABCC_BUILD_MODE__ ?? "dev",
    builtAt: g.__CRABCC_BUILD_TS__ ?? 0,
    gitSha: g.__CRABCC_GIT_SHA__ ?? "",
  };
}

/**
 * Generate a CSS selector that uniquely identifies `el` from `document`.
 * Prefers `#id` when present (and unique), otherwise builds a path of
 * `tag.class:nth-of-type(n)` segments walking up the parent chain.
 */
function cssSelectorFor(el: HTMLElement): string {
  if (el.id && document.querySelectorAll(`#${cssEscape(el.id)}`).length === 1) {
    return `#${cssEscape(el.id)}`;
  }
  const parts: string[] = [];
  let cur: Element | null = el;
  while (cur && cur.nodeType === 1 && cur !== document.body) {
    const tagName = cur.tagName;
    let seg = tagName.toLowerCase();
    if (cur.classList.length > 0) {
      seg += "." + Array.from(cur.classList).map(cssEscape).join(".");
    }
    const parent: Element | null = cur.parentElement;
    if (parent) {
      const sameTag = Array.from(parent.children).filter(
        (c: Element) => c.tagName === tagName,
      );
      if (sameTag.length > 1) {
        const idx = sameTag.indexOf(cur) + 1;
        seg += `:nth-of-type(${idx})`;
      }
    }
    parts.unshift(seg);
    cur = parent;
  }
  return parts.join(" > ") || el.tagName.toLowerCase();
}

function cssEscape(s: string): string {
  if (typeof CSS !== "undefined" && typeof CSS.escape === "function") {
    return CSS.escape(s);
  }
  return s.replace(/[^a-zA-Z0-9_-]/g, (c) => `\\${c}`);
}

let installed = false;
let errorHandlerAttached = false;

export function installDebugBridge(): CrabccDebugBridge {
  // Idempotent: HMR may re-run module init. Re-installing would clobber
  // listener subscriptions, so we keep the original `state` object alive
  // and just reattach to `window`.
  if (typeof window !== "undefined") {
    (window as unknown as { __crabcc__: CrabccDebugBridge }).__crabcc__ = state;
  }
  if (!installed) {
    installed = true;
    attachErrorCapture();
    if (typeof window !== "undefined") {
      // eslint-disable-next-line no-console
      console.log(
        "[crabcc] debug bridge installed at window.__crabcc__ (schema v%d)",
        state.schemaVersion,
        state,
      );
    }
  }
  return state;
}

function attachErrorCapture(): void {
  if (errorHandlerAttached) return;
  if (typeof window === "undefined") return;
  // Test envs (bun test) stub `globalThis.window = {}` — no addEventListener.
  // Fail closed there: skip capture rather than throw.
  if (typeof (window as Window).addEventListener !== "function") return;
  errorHandlerAttached = true;
  window.addEventListener("error", (ev) => {
    pushError({
      message: String(ev.message ?? ""),
      stack: ev.error?.stack ? String(ev.error.stack) : "",
      source: "error",
      ts: Date.now(),
    });
  });
  window.addEventListener("unhandledrejection", (ev) => {
    const reason = (ev as PromiseRejectionEvent).reason;
    pushError({
      message: typeof reason === "string" ? reason : String(reason?.message ?? reason),
      stack: reason?.stack ? String(reason.stack) : "",
      source: "unhandledrejection",
      ts: Date.now(),
    });
  });
}

/** Append an error to the ring buffer + notify subscribers. Public for manual reports. */
export function pushError(err: BridgeError): void {
  state.errors = [err, ...state.errors].slice(0, ERR_RING);
  notify();
}

/** Append a log entry to the ring buffer + notify subscribers. */
export function pushLog(entry: BridgeLog): void {
  state.logs = [entry, ...state.logs].slice(0, LOG_RING);
  notify();
}

function notify(): void {
  state.updatedAt = Date.now();
  for (const cb of listeners) {
    try {
      cb(state);
    } catch {
      // listener errors must not break the dashboard
    }
  }
}

/** Update the bridge state in-place; notify subscribers. */
export function updateDebugBridge(
  patch: Partial<Omit<CrabccDebugBridge, BridgeMethods>>,
): void {
  Object.assign(state, patch);
  notify();
}

// ---------------------------------------------------------------------------
// Browser-automation primitives (schema v2)
//
// These mirror the surface a typical browser-automation MCP exposes
// (navigate / hover / type / selectOption / drag / ariaSnapshot) so the
// MV3 extension can drive any page hosting the bridge without going back
// through `crabcc serve`. All methods are best-effort and never throw —
// they return `false` / null on any failure so a remote caller can branch
// on the result instead of catching exceptions across the bridge boundary.
// ---------------------------------------------------------------------------

function navigateTo(url: string): boolean {
  if (typeof window === "undefined") return false;
  try {
    window.location.href = url;
    return true;
  } catch {
    return false;
  }
}

function pressKey(key: string, opts: PressKeyOptions = {}): boolean {
  if (typeof document === "undefined") return false;
  let target: Element | null = null;
  if (opts.selector) {
    try {
      target = document.querySelector(opts.selector);
    } catch {
      return false;
    }
  }
  if (!target) {
    target =
      (document.activeElement as Element | null) ?? document.body ?? document.documentElement;
  }
  if (!target) return false;
  const mods = new Set(opts.modifiers ?? []);
  const init: KeyboardEventInit = {
    key,
    code: keyToCode(key),
    bubbles: true,
    cancelable: true,
    altKey: mods.has("Alt"),
    ctrlKey: mods.has("Control"),
    metaKey: mods.has("Meta"),
    shiftKey: mods.has("Shift"),
  };
  try {
    target.dispatchEvent(new KeyboardEvent("keydown", init));
    if (key.length === 1) {
      target.dispatchEvent(new KeyboardEvent("keypress", init));
      // Mirror the printable-character path: also fire input events on
      // editable targets so React onChange listeners pick it up.
      if (isEditable(target)) {
        const before = readEditableValue(target);
        writeEditableValue(target, before + key);
        target.dispatchEvent(new InputEvent("input", { bubbles: true, data: key }));
      }
    }
    target.dispatchEvent(new KeyboardEvent("keyup", init));
    return true;
  } catch {
    return false;
  }
}

function keyToCode(key: string): string {
  if (key.length === 1) {
    const c = key.toUpperCase();
    if (c >= "A" && c <= "Z") return `Key${c}`;
    if (c >= "0" && c <= "9") return `Digit${c}`;
  }
  // Already a "Code"-shaped value (Enter, Escape, ArrowLeft, …).
  return key;
}

function hoverBySelector(selector: string): boolean {
  if (typeof document === "undefined") return false;
  let el: HTMLElement | null = null;
  try {
    el = document.querySelector<HTMLElement>(selector);
  } catch {
    return false;
  }
  return el ? hoverElement(el) : false;
}

function hoverElement(el: HTMLElement): boolean {
  const rect = el.getBoundingClientRect();
  if (rect.width === 0 || rect.height === 0) return false;
  const cx = rect.left + rect.width / 2;
  const cy = rect.top + rect.height / 2;
  const init: MouseEventInit = {
    bubbles: true,
    cancelable: true,
    clientX: cx,
    clientY: cy,
    view: typeof window !== "undefined" ? window : undefined,
  };
  try {
    el.dispatchEvent(new MouseEvent("mouseover", init));
    el.dispatchEvent(new MouseEvent("mouseenter", { ...init, bubbles: false }));
    el.dispatchEvent(new MouseEvent("mousemove", init));
    return true;
  } catch {
    return false;
  }
}

function typeIntoSelector(
  selector: string,
  text: string,
  opts: TypeOptions = {},
): boolean {
  if (typeof document === "undefined") return false;
  let el: HTMLElement | null = null;
  try {
    el = document.querySelector<HTMLElement>(selector);
  } catch {
    return false;
  }
  return el ? typeIntoElement(el, text, opts) : false;
}

function typeIntoElement(el: HTMLElement, text: string, opts: TypeOptions = {}): boolean {
  if (!isEditable(el)) return false;
  try {
    if (typeof (el as HTMLElement).focus === "function") el.focus();
    const before = opts.append ? readEditableValue(el) : "";
    writeEditableValue(el, before + text);
    el.dispatchEvent(new InputEvent("input", { bubbles: true, data: text }));
    el.dispatchEvent(new Event("change", { bubbles: true }));
    if (opts.submit) {
      const enterInit: KeyboardEventInit = {
        key: "Enter",
        code: "Enter",
        bubbles: true,
        cancelable: true,
      };
      el.dispatchEvent(new KeyboardEvent("keydown", enterInit));
      el.dispatchEvent(new KeyboardEvent("keyup", enterInit));
      // Submit the surrounding form if any (mirrors how Enter behaves in
      // a real browser for a single-line input).
      const form = (el as HTMLInputElement).form;
      if (form && typeof form.requestSubmit === "function") {
        try {
          form.requestSubmit();
        } catch {
          // requestSubmit can throw if the form has no submit button or
          // is disconnected — fall through silently.
        }
      }
    }
    return true;
  } catch {
    return false;
  }
}

function isEditable(el: Element): el is HTMLElement {
  if (!(el instanceof HTMLElement)) return false;
  if (el.tagName === "INPUT" || el.tagName === "TEXTAREA") return true;
  if (el.isContentEditable) return true;
  return false;
}

function readEditableValue(el: Element): string {
  if (el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement) {
    return el.value;
  }
  return (el as HTMLElement).textContent ?? "";
}

function writeEditableValue(el: Element, value: string): void {
  // React tracks input values via a setter monkey-patched on the
  // prototype's `value` property — bypassing the patched setter is the
  // standard trick for synthetic typing.
  if (el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement) {
    const proto = Object.getPrototypeOf(el);
    const desc = proto && Object.getOwnPropertyDescriptor(proto, "value");
    if (desc && typeof desc.set === "function") {
      desc.set.call(el, value);
      return;
    }
    el.value = value;
    return;
  }
  (el as HTMLElement).textContent = value;
}

function selectOptionBySelector(selector: string, value: string): boolean {
  if (typeof document === "undefined") return false;
  let el: HTMLSelectElement | null = null;
  try {
    el = document.querySelector<HTMLSelectElement>(selector);
  } catch {
    return false;
  }
  if (!el || el.tagName !== "SELECT") return false;
  const opts = Array.from(el.options);
  const match =
    opts.find((o) => o.value === value) ?? opts.find((o) => o.text.trim() === value);
  if (!match) return false;
  try {
    el.value = match.value;
    el.dispatchEvent(new Event("input", { bubbles: true }));
    el.dispatchEvent(new Event("change", { bubbles: true }));
    return true;
  } catch {
    return false;
  }
}

function dragBetweenSelectors(startSelector: string, endSelector: string): boolean {
  if (typeof document === "undefined") return false;
  let from: HTMLElement | null = null;
  let to: HTMLElement | null = null;
  try {
    from = document.querySelector<HTMLElement>(startSelector);
    to = document.querySelector<HTMLElement>(endSelector);
  } catch {
    return false;
  }
  if (!from || !to) return false;
  const fromRect = from.getBoundingClientRect();
  const toRect = to.getBoundingClientRect();
  if (fromRect.width === 0 || toRect.width === 0) return false;
  const fx = fromRect.left + fromRect.width / 2;
  const fy = fromRect.top + fromRect.height / 2;
  const tx = toRect.left + toRect.width / 2;
  const ty = toRect.top + toRect.height / 2;
  // DataTransfer is required by HTML5 drag events; the constructor is
  // available in modern Chrome but may throw elsewhere.
  let dt: DataTransfer | null = null;
  try {
    dt = new DataTransfer();
  } catch {
    dt = null;
  }
  const init = (clientX: number, clientY: number): DragEventInit => ({
    bubbles: true,
    cancelable: true,
    clientX,
    clientY,
    dataTransfer: dt ?? undefined,
  });
  try {
    from.dispatchEvent(new DragEvent("dragstart", init(fx, fy)));
    to.dispatchEvent(new DragEvent("dragenter", init(tx, ty)));
    to.dispatchEvent(new DragEvent("dragover", init(tx, ty)));
    to.dispatchEvent(new DragEvent("drop", init(tx, ty)));
    from.dispatchEvent(new DragEvent("dragend", init(tx, ty)));
    return true;
  } catch {
    return false;
  }
}

// --- ARIA snapshot --------------------------------------------------------
//
// The snapshot is a synthetic accessibility tree: each visited element
// gets a fresh `ref` id (`r{counter}`) and is registered in `refMap` so
// `clickByRef` / `typeByRef` / `hoverByRef` can resolve back to a live
// element without re-querying. Refs are invalidated on every new
// snapshot — the extension is expected to re-snapshot before acting.
// We deliberately don't compute the full WAI-ARIA accessible name (that
// requires implementing the full algorithm); instead we use a small
// pragmatic hierarchy: aria-label > aria-labelledby > label-for /
// associated <label> > visible text > placeholder > title.

const ARIA_SKIP = new Set(["SCRIPT", "STYLE", "NOSCRIPT", "TEMPLATE"]);
const refMap = new Map<string, WeakRef<HTMLElement>>();
let refCounter = 0;

function takeAriaSnapshot(opts: AriaSnapshotOptions = {}): AriaNode {
  refMap.clear();
  refCounter = 0;
  if (typeof document === "undefined") {
    return emptyNode();
  }
  const root = document.body ?? document.documentElement;
  if (!root) return emptyNode();
  return walkAria(root as HTMLElement, 0, opts);
}

function emptyNode(): AriaNode {
  return {
    ref: "",
    role: "document",
    name: "",
    tag: "html",
    focusable: false,
    visible: false,
    x: 0,
    y: 0,
    width: 0,
    height: 0,
    children: [],
  };
}

function walkAria(el: HTMLElement, depth: number, opts: AriaSnapshotOptions): AriaNode {
  const ref = `r${++refCounter}`;
  refMap.set(ref, new WeakRef(el));
  let rect: DOMRect;
  try {
    rect = el.getBoundingClientRect();
  } catch {
    rect = new DOMRect(0, 0, 0, 0);
  }
  const visible = rect.width > 0 && rect.height > 0;
  const node: AriaNode = {
    ref,
    role: ariaRole(el),
    name: ariaName(el),
    tag: el.tagName.toLowerCase(),
    focusable: isFocusable(el),
    visible,
    x: Math.round(rect.left + (typeof window !== "undefined" ? window.scrollX : 0)),
    y: Math.round(rect.top + (typeof window !== "undefined" ? window.scrollY : 0)),
    width: Math.round(rect.width),
    height: Math.round(rect.height),
    children: [],
  };
  const cap = opts.maxDepth;
  if (cap !== undefined && depth >= cap) return node;
  for (const child of Array.from(el.children) as HTMLElement[]) {
    if (ARIA_SKIP.has(child.tagName)) continue;
    if (opts.visibleOnly) {
      const cr = child.getBoundingClientRect();
      if (cr.width === 0 || cr.height === 0) continue;
    }
    node.children.push(walkAria(child, depth + 1, opts));
  }
  return node;
}

const IMPLICIT_ROLES: Record<string, string> = {
  A: "link",
  BUTTON: "button",
  NAV: "navigation",
  MAIN: "main",
  HEADER: "banner",
  FOOTER: "contentinfo",
  ASIDE: "complementary",
  ARTICLE: "article",
  SECTION: "region",
  FORM: "form",
  IMG: "img",
  UL: "list",
  OL: "list",
  LI: "listitem",
  H1: "heading",
  H2: "heading",
  H3: "heading",
  H4: "heading",
  H5: "heading",
  H6: "heading",
  TEXTAREA: "textbox",
  SELECT: "combobox",
  TABLE: "table",
  TR: "row",
  TD: "cell",
  TH: "columnheader",
  DIALOG: "dialog",
};

function ariaRole(el: HTMLElement): string {
  const explicit = el.getAttribute("role");
  if (explicit) return explicit;
  if (el.tagName === "INPUT") {
    const t = (el as HTMLInputElement).type || "text";
    if (t === "submit" || t === "button" || t === "reset") return "button";
    if (t === "checkbox") return "checkbox";
    if (t === "radio") return "radio";
    if (t === "search") return "searchbox";
    if (t === "range") return "slider";
    return "textbox";
  }
  if (el.tagName === "A" && !el.hasAttribute("href")) return "generic";
  return IMPLICIT_ROLES[el.tagName] ?? "generic";
}

function ariaName(el: HTMLElement): string {
  const direct = el.getAttribute("aria-label");
  if (direct) return direct.trim().slice(0, 120);
  const labelledBy = el.getAttribute("aria-labelledby");
  if (labelledBy) {
    const parts = labelledBy
      .split(/\s+/)
      .map((id) => document.getElementById(id)?.textContent ?? "")
      .filter(Boolean);
    if (parts.length) return parts.join(" ").trim().slice(0, 120);
  }
  if (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.tagName === "SELECT") {
    const id = el.id;
    if (id) {
      const lbl = document.querySelector<HTMLLabelElement>(
        `label[for="${cssEscape(id)}"]`,
      );
      if (lbl?.textContent) return lbl.textContent.trim().slice(0, 120);
    }
    const wrap = el.closest("label");
    if (wrap?.textContent) return wrap.textContent.trim().slice(0, 120);
    const ph = (el as HTMLInputElement).placeholder;
    if (ph) return ph.trim().slice(0, 120);
  }
  if (el.tagName === "IMG") {
    const alt = (el as HTMLImageElement).alt;
    if (alt) return alt.trim().slice(0, 120);
  }
  const text = (el.textContent ?? "").replace(/\s+/g, " ").trim();
  if (text) return text.slice(0, 120);
  const title = el.getAttribute("title");
  return title ? title.trim().slice(0, 120) : "";
}

function isFocusable(el: HTMLElement): boolean {
  const tag = el.tagName;
  if (tag === "A") return el.hasAttribute("href");
  if (tag === "BUTTON" || tag === "SELECT" || tag === "TEXTAREA") return true;
  if (tag === "INPUT") return (el as HTMLInputElement).type !== "hidden";
  const ti = el.getAttribute("tabindex");
  return ti !== null && ti !== "-1";
}

function resolveRef(ref: string): HTMLElement | null {
  const w = refMap.get(ref);
  if (!w) return null;
  const el = w.deref() ?? null;
  if (!el || !el.isConnected) return null;
  return el;
}
