// Compatibility shim — the implementation lives under `./activity/`.
// Kept so existing `import { ActivityPanel } from "../components/ActivityPanel"`
// call sites don't need to change. Delete the shim once nothing imports
// from this path anymore.
export { ActivityPanel } from "./activity";
