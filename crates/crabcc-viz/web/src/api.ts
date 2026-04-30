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

export const api = {
  bootstrap: () => getJson<Bootstrap>("/api/bootstrap"),
  activity: (sinceTs?: number, limit = 100) =>
    getJson<{ items: ActivityHit[] }>(
      `/api/activity?since=${sinceTs ?? 0}&limit=${limit}`,
    ),
  agents: () => getJson<{ agents: AgentSummary[] }>("/api/agents"),
  agentLog: (id: string, since: number) =>
    getJson<AgentLog>(`/api/agents/${id}/log?since=${since}`),
  reindex: () => postJson<ReindexReport>("/api/reindex"),
  randomQuery: () =>
    postJson<{ op: string; symbol: string }>("/api/random-query"),
  telemetry: (sinceTs?: number, limit = 100) =>
    getJson<TelemetrySnapshot>(
      `/api/telemetry?since=${sinceTs ?? 0}&limit=${limit}`,
    ),
};
