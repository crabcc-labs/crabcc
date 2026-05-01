// Compatibility shim — the implementation lives under `./agents/`.
// The consolidated tabbed panel owns live agents + profiles + kills +
// models internally, so the four legacy `Agent{Profiles,Kills,Models}Panel`
// files (and `AgentLogView`) can stay deleted; this re-export keeps
// the App.tsx call site stable.
export { AgentsPanel } from "./agents";
