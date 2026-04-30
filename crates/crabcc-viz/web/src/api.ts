// Thin typed client over the existing /api/* endpoints. One module per
// endpoint family — keeps callers free of fetch boilerplate and routes
// JSON parsing through a single typed surface so a server-side schema
// drift surfaces here, not deep in a component.

export type Bootstrap = {
  repo: string;
  root: string;
  version: string;
  index?: { present: boolean; files?: number; symbols?: number };
};

export type ActivityHit = {
  ts: number;
  op: string;
  query: string;
  count: number;
  source?: string;
};

export type AgentSummary = {
  id: string;
  status: "running" | "exited";
  pid?: number;
  prompt_preview?: string;
  model?: string;
  started_ts?: number;
  exit_code?: number | null;
};

export type AgentLog = {
  body: string;
  cursor: number;
  total: number;
};

export type ReindexReport = {
  root: string;
  elapsed_ms: number;
  stats: Record<string, number | string>;
  logs: string[];
};

async function getJson<T>(path: string): Promise<T> {
  const r = await fetch(path);
  if (!r.ok) throw new Error(`${r.status} ${r.statusText}`);
  return (await r.json()) as T;
}

async function postJson<T>(path: string, body?: unknown): Promise<T> {
  const r = await fetch(path, {
    method: "POST",
    headers: body ? { "Content-Type": "application/json" } : undefined,
    body: body ? JSON.stringify(body) : undefined,
  });
  if (!r.ok) throw new Error(`${r.status} ${r.statusText}: ${await r.text()}`);
  return (await r.json()) as T;
}

// Issue #90 → dashboard surface: telemetry events written by every
// crabcc invocation through the JSON file layer in crabcc-cli's
// telemetry::init(). One line per event; the dashboard polls every
// few seconds and re-renders the panel.
export type TelemetryEvent = {
  ts: number;       // unix seconds, parsed from the ISO8601 timestamp
  level: string;    // INFO / WARN / ERROR / DEBUG / TRACE
  target: string;   // e.g. crabcc_core::graph
  fields: Record<string, unknown>;
};

export type TelemetrySource = {
  path: string;
  lines_read: number;
  bytes: number;
  exists: boolean;
};

export type TelemetrySnapshot = {
  cursor: number;
  events: TelemetryEvent[];
  source: TelemetrySource;
};

// Issue #112 follow-up — agent dashboard surfaces.
export type AgentProfileEntry = {
  id: string;
  crate_: string | null;
  description: string | null;
  model: string | null;
};
export type AgentKillRow = {
  run_id: string;
  reason: string;
  pid: number | null;
  detail: string | null;
  killed_at: number;
};
export type AgentModelEntry = {
  file: string;
  provider: string;
  name: string;
  params: string | null;
  context: number | null;
  docs_first: string | null;
};

export const api = {
  bootstrap: () => getJson<Bootstrap>("/api/bootstrap"),
  activity: (sinceTs?: number, limit = 100) =>
    getJson<{ items: ActivityHit[] }>(
      `/api/activity?since=${sinceTs ?? 0}&limit=${limit}`,
    ),
  agents: () => getJson<{ agents: AgentSummary[] }>("/api/agents"),
  agentLog: (id: string, since: number) =>
    getJson<AgentLog>(`/api/agents/${id}/log?since=${since}`),
  agentProfiles: () =>
    getJson<{ dir: string; profiles: AgentProfileEntry[] }>(
      "/api/agent-profiles",
    ),
  agentKills: () =>
    getJson<{ db: string; rows: AgentKillRow[] }>("/api/agent-kills"),
  agentModels: () =>
    getJson<{ dir: string; models: AgentModelEntry[] }>("/api/agent-models"),
  reindex: () => postJson<ReindexReport>("/api/reindex"),
  randomQuery: () =>
    postJson<{ op: string; symbol: string }>("/api/random-query"),
  telemetry: (sinceTs?: number, limit = 100) =>
    getJson<TelemetrySnapshot>(
      `/api/telemetry?since=${sinceTs ?? 0}&limit=${limit}`,
    ),
};

// Issue #112 follow-up — expose the api surface on window for
// Chrome DevTools console use. Devs can call `window.crabcc.agents()`,
// `window.crabcc.agentKills()`, etc. directly. Logged once on first
// import so the console hint shows up before the React tree mounts.
declare global {
  interface Window {
    crabcc?: typeof api;
  }
}
if (typeof window !== "undefined") {
  window.crabcc = api;
  // Friendly console banner — readable in Chrome DevTools / Safari Web Inspector.
  // eslint-disable-next-line no-console
  console.info(
    "%ccrabcc%c live dashboard — call %cwindow.crabcc%c.{agents, agentProfiles, agentKills, agentModels, telemetry, ...}() to query the running server.",
    "color:#ff2a6d;font-weight:700;text-shadow:0 0 4px rgba(255,42,109,.5);",
    "color:#c8d4ff;",
    "color:#00f0ff;font-weight:700;",
    "color:#c8d4ff;",
  );
}
