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

const initial: Omit<CrabccDebugBridge, "subscribe"> = {
  schemaVersion: 1,
  appVersion: "dev",
  apiBase: "http://localhost:7878",
  repoRoot: null,
  agentCount: 0,
  activityCount: 0,
  updatedAt: Date.now(),
};

const state: CrabccDebugBridge = {
  ...initial,
  subscribe(cb) {
    listeners.add(cb);
    cb(state);
    return () => {
      listeners.delete(cb);
    };
  },
  buttons: snapshotInteractives,
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
  const els = document.querySelectorAll<HTMLElement>(INTERACTIVE_SELECTOR);
  els.forEach((el) => {
    const rect = el.getBoundingClientRect();
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
    let seg = cur.tagName.toLowerCase();
    if (cur.classList.length > 0) {
      seg += "." + Array.from(cur.classList).map(cssEscape).join(".");
    }
    const parent = cur.parentElement;
    if (parent) {
      const sameTag = Array.from(parent.children).filter(
        (c) => c.tagName === cur!.tagName,
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
  // Minimal CSS.escape polyfill — most browsers ship CSS.escape natively;
  // fall back to a regex for the test environment + ancient browsers.
  if (typeof CSS !== "undefined" && typeof CSS.escape === "function") {
    return CSS.escape(s);
  }
  return s.replace(/[^a-zA-Z0-9_-]/g, (c) => `\\${c}`);
}

export function installDebugBridge(): CrabccDebugBridge {
  (window as unknown as { __crabcc__: CrabccDebugBridge }).__crabcc__ = state;
  // Visible breadcrumb in DevTools — confirms the bridge mounted and lets
  // someone inspecting the page eyeball the schema without grepping source.
  console.log(
    "[crabcc] debug bridge installed at window.__crabcc__ (schema v%d)",
    state.schemaVersion,
    state,
  );
  return state;
}

/** Update the bridge state in-place; notify subscribers. */
export function updateDebugBridge(patch: Partial<Omit<CrabccDebugBridge, "subscribe">>): void {
  Object.assign(state, patch, { updatedAt: Date.now() });
  for (const cb of listeners) {
    try {
      cb(state);
    } catch {
      // listener errors must not break the dashboard
    }
  }
}
