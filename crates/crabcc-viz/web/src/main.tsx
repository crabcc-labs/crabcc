import { lazy, Suspense, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { installDebugBridge } from "./debugBridge";
import { type Route, routeFor } from "./router";
import "./styles.css";

// Mount the `window.__crabcc__` debug bridge before the React tree so
// it's available even on first paint. The Chrome extension (#184) reads
// from here via `chrome.scripting.executeScript`.
installDebugBridge();

// Code-split the knowledge view: only on screen at #/knowledge. The
// dashboard chunk doesn't pull in the knowledge module, so first paint
// of `/` stays cheap.
const KnowledgeView = lazy(() =>
  import("./components/knowledge").then((m) => ({ default: m.KnowledgeView })),
);

function Router() {
  const [route, setRoute] = useState<Route>(() =>
    routeFor(typeof window !== "undefined" ? window.location.hash : ""),
  );
  useEffect(() => {
    const onHash = () => setRoute(routeFor(window.location.hash));
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);
  if (route === "knowledge") {
    return (
      <Suspense fallback={<div className="placeholder">loading knowledge view…</div>}>
        <KnowledgeView />
      </Suspense>
    );
  }
  return <App />;
}

const container = document.getElementById("root");
if (!container) {
  throw new Error("missing #root element");
}
createRoot(container).render(<Router />);
