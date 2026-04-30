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
};
