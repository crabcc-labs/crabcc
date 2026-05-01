// Agents-panel-internal types. Wire-shape types come from the OpenAPI
// codegen (`AgentSummary`, `AgentProfileEntry`, `AgentKillRow`,
// `AgentModelEntry`); this module declares the *display* models we
// derive from them: the four-tab id, the kill row grouped by day,
// and the per-tab filter state.

import type {
  AgentSummary,
  AgentProfileEntry,
  AgentKillRow,
  AgentModelEntry,
} from "../../api";

export type {
  AgentSummary,
  AgentProfileEntry,
  AgentKillRow,
  AgentModelEntry,
};

/// Four tabs at the top of the consolidated agents panel.
export type TabId = "live" | "profiles" | "kills" | "models";

export const TAB_ORDER: TabId[] = ["live", "profiles", "kills", "models"];

/// Sort modes for the live agents tab. The default is "started" newest
/// first (matches the previous behaviour where SSE pushed the freshest
/// first); status / uptime are user-toggled secondaries.
export type LiveSort = "started" | "status" | "uptime";

/// A "row" in the kills feed — either a sticky day header ("Today",
/// "Yesterday", "Apr 30") or a literal kill event.
export type KillRow =
  | { kind: "header"; key: string; label: string }
  | { kind: "kill"; key: string; row: AgentKillRow };
