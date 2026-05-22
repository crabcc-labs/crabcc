// Tiny dependency-free hash router. The dashboard ships as a single
// `include_str!`-baked HTML bundle, so server-side routing isn't an
// option; hash routing keeps deep-links working without a build-time
// bundler change. Route values are deliberately small — adding a new
// view is `routeFor("#/foo") => "foo"` plus a `<Router>` switch arm.

import { useSyncExternalStore } from "react";

export type Route = "dashboard" | "logs" | "system" | "knowledge" | "prs" | "analytics";

/** Map a raw `window.location.hash` onto our internal route enum. */
export function routeFor(hash: string): Route {
  // Trim leading `#` (and the conventional `#/`) so the comparison is
  // tolerant of either `#knowledge` or `#/knowledge` from a user-typed URL.
  // Strip query suffix (`#/logs?event=…` → `logs`) so a deep-linked filter
  // still resolves to the right view.
  const clean = hash.replace(/^#\/?/, "").split("?")[0]!;
  if (clean === "knowledge") return "knowledge";
  if (clean === "logs") return "logs";
  if (clean === "system") return "system";
  if (clean.startsWith("prs")) return "prs";
  if (clean === "analytics") return "analytics";
  return "dashboard";
}

/**
 * Navigate to a route by mutating `window.location.hash`. The browser
 * will fire a `hashchange` event afterward, which `useRoute` listens
 * for — so callers don't need to do anything else to trigger a re-render.
 */
export function navigate(route: Route, sub?: string): void {
  if (typeof window === "undefined") return;
  const base = route === "dashboard" ? "" : `#/${route}`;
  const target = sub ? `${base}/${sub}` : base;
  if (window.location.hash !== target) {
    window.location.hash = target;
  }
}

/** For routes like #/prs/42, extract the sub-segment ("42"). */
export function routeSub(): string {
  if (typeof window === "undefined") return "";
  const hash = window.location.hash.replace(/^#\/?/, "");
  const parts = hash.split("/");
  return parts.length >= 2 ? parts[1]! : "";
}

// ── React subscription -------------------------------------------------------

function readHash(): string {
  return typeof window === "undefined" ? "" : window.location.hash;
}

function subscribe(cb: () => void): () => void {
  if (typeof window === "undefined") return () => {};
  window.addEventListener("hashchange", cb);
  return () => window.removeEventListener("hashchange", cb);
}

/** Subscribe to the live hash route. Re-renders only when the route changes. */
export function useRoute(): Route {
  const hash = useSyncExternalStore(subscribe, readHash, readHash);
  return routeFor(hash);
}

/**
 * Read the trailing query-string off a hash route (e.g. `#/logs?event=12`
 * → `event=12`). Returned `URLSearchParams` so callers can `.get(key)`
 * without re-parsing.
 */
export function hashQuery(): URLSearchParams {
  if (typeof window === "undefined") return new URLSearchParams();
  const ix = window.location.hash.indexOf("?");
  if (ix < 0) return new URLSearchParams();
  return new URLSearchParams(window.location.hash.slice(ix + 1));
}
