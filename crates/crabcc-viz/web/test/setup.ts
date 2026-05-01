// Registers a real `happy-dom` `window` / `document` / `navigator` /
// `HTMLElement` etc. on `globalThis` BEFORE any test file imports
// React. Wired via `bunfig.toml` (`[test] preload = ["./test/setup.ts"]`)
// so component tests can render into a real DOM instead of stubbing
// the globals by hand. Pure-logic tests (store reducers etc.) keep
// working unchanged — they ignore the globals.

import { GlobalRegistrator } from "@happy-dom/global-registrator";

if (!GlobalRegistrator.isRegistered) {
  GlobalRegistrator.register({
    // Loopback URL so any `window.location.origin`-relative fetch
    // target lines up with the dashboard's actual surface.
    url: "http://127.0.0.1:7878/",
    width: 1280,
    height: 800,
  });
}

// React 19 requires this flag set for `act()` to flush updates
// silently; without it React warns + bails on flushing pending work
// inside the act callback.
(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
