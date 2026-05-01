import { createRoot } from "react-dom/client";
import { App } from "./App";
import { installDebugBridge } from "./debugBridge";
import "./styles.css";

// Mount the `window.__crabcc__` debug bridge before the React tree so
// it's available even on first paint. The Chrome extension (#184) reads
// from here via `chrome.scripting.executeScript`.
installDebugBridge();

const container = document.getElementById("root");
if (!container) {
  throw new Error("missing #root element");
}
createRoot(container).render(<App />);
