// Pure transforms for the agents module — no React, no DOM. Tested
// directly under bun test. Mirrors the activity module's store split.
//
// Each tab has its own filter+sort logic; the kills tab additionally
// buckets by local-day so the feed reads "Today / Yesterday / Apr 30 …".

import type {
  AgentSummary,
  AgentProfileEntry,
  AgentKillRow,
  AgentModelEntry,
  LiveSort,
  KillRow,
} from "./types";

// ── live agents ───────────────────────────────────────────────────────

/// Case-insensitive substring match against id, prompt preview, model,
/// pid (formatted as decimal), or status. Empty filter returns input by
/// reference so React's referential-equality short-circuits hold.
export function filterAgents(
  agents: AgentSummary[],
  text: string,
): AgentSummary[] {
  const q = text.trim().toLowerCase();
  if (!q) return agents;
  return agents.filter((a) => {
    if (a.id.toLowerCase().includes(q)) return true;
    if ((a.prompt_preview ?? "").toLowerCase().includes(q)) return true;
    if ((a.model ?? "").toLowerCase().includes(q)) return true;
    if (a.status.includes(q)) return true;
    if (a.pid !== undefined && String(a.pid).includes(q)) return true;
    return false;
  });
}

/// Stable sort by mode. `now` is wall-clock seconds (uptime depends on
/// it). Returns a *new* array; never mutates the input.
export function sortAgents(
  agents: AgentSummary[],
  mode: LiveSort,
  now: number,
): AgentSummary[] {
  const out = agents.slice();
  switch (mode) {
    case "started":
      out.sort((a, b) => (b.started_ts ?? 0) - (a.started_ts ?? 0));
      break;
    case "status":
      // Running first, then exited; tiebreak on started_ts (newest).
      out.sort((a, b) => {
        const sa = a.status === "running" ? 0 : 1;
        const sb = b.status === "running" ? 0 : 1;
        if (sa !== sb) return sa - sb;
        return (b.started_ts ?? 0) - (a.started_ts ?? 0);
      });
      break;
    case "uptime":
      // Longest-running first. Exited rows fall to the bottom regardless
      // of how long they ran (their "uptime" is undefined now).
      out.sort((a, b) => {
        const ua = a.status === "running" ? now - (a.started_ts ?? now) : -1;
        const ub = b.status === "running" ? now - (b.started_ts ?? now) : -1;
        return ub - ua;
      });
      break;
  }
  return out;
}

/// Format an agent's uptime as a short string. Exited agents return "—".
export function uptimeLabel(a: AgentSummary, now: number): string {
  if (a.status !== "running") return "—";
  if (a.started_ts === undefined) return "?";
  const age = Math.max(0, now - a.started_ts);
  if (age < 60) return `${age}s`;
  if (age < 3600) return `${Math.floor(age / 60)}m`;
  if (age < 86400) return `${Math.floor(age / 3600)}h`;
  return `${Math.floor(age / 86400)}d`;
}

// ── profiles ──────────────────────────────────────────────────────────

/// Filter profiles on id, crate, model, or description.
export function filterProfiles(
  profiles: AgentProfileEntry[],
  text: string,
): AgentProfileEntry[] {
  const q = text.trim().toLowerCase();
  if (!q) return profiles;
  return profiles.filter((p) => {
    if (p.id.toLowerCase().includes(q)) return true;
    if ((p.crate_ ?? "").toLowerCase().includes(q)) return true;
    if ((p.model ?? "").toLowerCase().includes(q)) return true;
    if ((p.description ?? "").toLowerCase().includes(q)) return true;
    return false;
  });
}

/// Profile ids currently referenced by a running agent. The wire shape
/// uses the agent's `prompt_preview` as a free-form string, so we can't
/// reliably resolve "is X in use" without a server-side join — instead
/// we conservatively match profile.id against any running agent's id
/// or model. False negatives are fine (we just don't decorate); false
/// positives would be visually loud, so we keep the match strict.
export function profilesInUse(
  profiles: AgentProfileEntry[],
  agents: AgentSummary[],
): Set<string> {
  const running = agents.filter((a) => a.status === "running");
  const ids = new Set<string>();
  for (const p of profiles) {
    for (const a of running) {
      if (a.id === p.id) {
        ids.add(p.id);
        break;
      }
      if (p.model && a.model && p.model === a.model) {
        ids.add(p.id);
        break;
      }
    }
  }
  return ids;
}

// ── kills ─────────────────────────────────────────────────────────────

/// Filter kill events on run id, reason, pid, or detail.
export function filterKills(
  rows: AgentKillRow[],
  text: string,
): AgentKillRow[] {
  const q = text.trim().toLowerCase();
  if (!q) return rows;
  return rows.filter((r) => {
    if (r.run_id.toLowerCase().includes(q)) return true;
    if (r.reason.toLowerCase().includes(q)) return true;
    if (r.pid !== null && String(r.pid).includes(q)) return true;
    if ((r.detail ?? "").toLowerCase().includes(q)) return true;
    return false;
  });
}

/// Bucket label for a unix-seconds timestamp, given the local "today"
/// midnight as a unix-seconds reference. Returns "Today", "Yesterday",
/// "Mon", "Apr 28", etc.
export function dayBucketFor(ts: number, todayMidnight: number): string {
  const ONE_DAY = 86_400;
  if (ts >= todayMidnight) return "Today";
  if (ts >= todayMidnight - ONE_DAY) return "Yesterday";
  if (ts >= todayMidnight - 7 * ONE_DAY) {
    return new Date(ts * 1000).toLocaleDateString(undefined, {
      weekday: "short",
    });
  }
  return new Date(ts * 1000).toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
  });
}

/// Compute the local-midnight unix-seconds value for `now`. Pure — only
/// reads `Date` for its side-table-free calendar arithmetic.
export function todayMidnightSecs(nowSecs: number): number {
  const d = new Date(nowSecs * 1000);
  d.setHours(0, 0, 0, 0);
  return Math.floor(d.getTime() / 1000);
}

/// Insert sticky day headers between kill rows. Input must be
/// reverse-chronological (newest first); output preserves order.
export function withDayHeaders(
  rows: AgentKillRow[],
  todayMidnight: number,
): KillRow[] {
  const out: KillRow[] = [];
  let lastBucket: string | null = null;
  for (const r of rows) {
    const bucket = dayBucketFor(r.killed_at, todayMidnight);
    if (bucket !== lastBucket) {
      out.push({ kind: "header", key: `h:${bucket}`, label: bucket });
      lastBucket = bucket;
    }
    out.push({
      kind: "kill",
      key: `${r.run_id}:${r.killed_at}`,
      row: r,
    });
  }
  return out;
}

// ── models ────────────────────────────────────────────────────────────

/// Filter models on provider / name / params.
export function filterModels(
  models: AgentModelEntry[],
  text: string,
): AgentModelEntry[] {
  const q = text.trim().toLowerCase();
  if (!q) return models;
  return models.filter((m) => {
    if (m.provider.toLowerCase().includes(q)) return true;
    if (m.name.toLowerCase().includes(q)) return true;
    if ((m.params ?? "").toLowerCase().includes(q)) return true;
    return false;
  });
}

/// Sort models by provider then name — gives a stable presentation
/// across re-polls regardless of the on-disk file order.
export function sortModels(models: AgentModelEntry[]): AgentModelEntry[] {
  return models.slice().sort((a, b) => {
    if (a.provider !== b.provider) return a.provider.localeCompare(b.provider);
    return a.name.localeCompare(b.name);
  });
}

/// Stable identifier for a model row — used as a list key and as the
/// detail-expansion anchor.
export function modelKey(m: AgentModelEntry): string {
  return `${m.provider}:${m.name}`;
}
