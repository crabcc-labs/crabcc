import { createRoot } from "react-dom/client";
import { App } from "./App";
import { installDebugBridge } from "./debugBridge";
// Tailwind v4 generated bundle (track B.1) — must come BEFORE the
// legacy styles.css so any rule there can override Tailwind's
// `@layer base` reset during the gradual migration. The generated
// file is produced by `bun run tailwind:build` and gitignored.
import "./tailwind.generated.css";
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
