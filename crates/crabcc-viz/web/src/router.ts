// Tiny dependency-free hash router. The dashboard ships as a single
// `include_str!`-baked HTML bundle, so server-side routing isn't an
// option; hash routing keeps deep-links working without a build-time
// bundler change. Route values are deliberately small — adding a new
// view is `routeFor("#/foo") => "foo"` plus a `<Router>` switch arm.

export type Route = "dashboard" | "knowledge";

/** Map a raw `window.location.hash` onto our internal route enum. */
export function routeFor(hash: string): Route {
  // Trim leading `#` (and the conventional `#/`) so the comparison is
  // tolerant of either `#knowledge` or `#/knowledge` from a user-typed URL.
  const clean = hash.replace(/^#\/?/, "");
  if (clean === "knowledge") return "knowledge";
  return "dashboard";
}

/**
 * Navigate to a route by mutating `window.location.hash`. The browser
 * will fire a `hashchange` event afterward, which `useRoute` listens
 * for — so callers don't need to do anything else to trigger a re-render.
 */
export function navigate(route: Route): void {
  if (typeof window === "undefined") return;
  const target = route === "dashboard" ? "" : `#/${route}`;
  if (window.location.hash !== target) {
    window.location.hash = target;
  }
}
