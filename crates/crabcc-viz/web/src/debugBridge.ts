// `window.__crabcc__` — debug bridge surfaced by the live-web app so the
// Chrome MV3 extension (#184) can attach to the dashboard tab and read
// dashboard state directly via `chrome.scripting.executeScript`, without
// going back through `crabcc serve`'s HTTP API.
//
// Schema is intentionally tiny — anything richer should live behind the
// existing `/api/*` routes. The extension subscribes to changes via
// `subscribe()` (returns an unsubscribe function), which avoids polling
// from the extension side.
//
// Versioned (`schemaVersion`) so the extension can refuse to attach
// against an incompatible dashboard build.

export interface CrabccDebugBridge {
  schemaVersion: 1;
  appVersion: string;
  apiBase: string;
  /** Repo path the dashboard is anchored to. */
  repoRoot: string | null;
  /** Last-known number of running agents. */
  agentCount: number;
  /** Last-known activity tail length. */
  activityCount: number;
  /** Snapshot timestamp (Date.now()). */
  updatedAt: number;
  /** Subscribe to state updates. Returns an unsubscribe function. */
  subscribe: (cb: (snapshot: CrabccDebugBridge) => void) => () => void;
  /**
   * Snapshot every interactive element in the current document
   * (buttons, links, role="button", inputs[type=submit]) with enough
   * identifying info for the Chrome extension to click them remotely
   * via `chrome.scripting.executeScript({ func, args })`.
   */
  buttons: () => CrabccInteractive[];
  /**
   * Click an element by CSS selector. Returns true iff a visible
   * element was found and `.click()` invoked. Safer than asking the
   * extension to synthesize a real mouse event because it short-circuits
   * when the element isn't currently in the DOM.
   */
  click: (selector: string) => boolean;
  /** Ring buffer of recent uncaught errors (last 16). */
  errors: BridgeError[];
}

export interface BridgeError {
  message: string;
  stack: string;
  /** "error" | "unhandledrejection" | "manual". */
  source: string;
  /** Date.now() when caught. */
  ts: number;
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

type Listener = (snap: CrabccDebugBridge) => void;
const listeners = new Set<Listener>();
// Cap to defend against an extension leaking subscriptions across
// reloads — silently dropping a 33rd subscribe is better than a UAF.
const MAX_LISTENERS = 32;
const ERR_RING = 16;

const initial: Omit<CrabccDebugBridge, "subscribe" | "buttons" | "click"> = {
  schemaVersion: 1,
  appVersion: "dev",
  apiBase: "http://localhost:7878",
  repoRoot: null,
  agentCount: 0,
  activityCount: 0,
  errors: [],
  updatedAt: Date.now(),
};

const state: CrabccDebugBridge = {
  ...initial,
  subscribe(cb) {
    if (listeners.size >= MAX_LISTENERS) return () => {};
    listeners.add(cb);
    try {
      cb(state);
    } catch {
      // initial fire must not break the subscribe contract
    }
    return () => {
      listeners.delete(cb);
    };
  },
  buttons: snapshotInteractives,
  click: clickBySelector,
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

function cssEscape(s: string): string {
  // Minimal CSS.escape polyfill — most browsers ship CSS.escape natively;
  // fall back to a regex for the test environment + ancient browsers.
  if (typeof CSS !== "undefined" && typeof CSS.escape === "function") {
    return CSS.escape(s);
  }
  return s.replace(/[^a-zA-Z0-9_-]/g, (c) => `\\${c}`);
}

let installed = false;

export function installDebugBridge(): CrabccDebugBridge {
  if (typeof window !== "undefined") {
    (window as unknown as { __crabcc__: CrabccDebugBridge }).__crabcc__ = state;
  }
  // Idempotent: HMR / re-imports run module init twice. Reattach to
  // window every time, but only attach the global error capture + log
  // banner once.
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
  if (typeof window === "undefined") return;
  // Test envs (bun test) stub `globalThis.window = {}` — no addEventListener.
  // Fail closed there: skip capture rather than throw.
  if (typeof (window as Window).addEventListener !== "function") return;
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
      message:
        typeof reason === "string"
          ? reason
          : String(reason?.message ?? reason),
      stack: reason?.stack ? String(reason.stack) : "",
      source: "unhandledrejection",
      ts: Date.now(),
    });
  });
}

/** Append an error to the ring buffer + notify subscribers. */
export function pushError(err: BridgeError): void {
  state.errors = [err, ...state.errors].slice(0, ERR_RING);
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
  patch: Partial<Omit<CrabccDebugBridge, "subscribe" | "buttons" | "click">>,
): void {
  Object.assign(state, patch);
  notify();
}
