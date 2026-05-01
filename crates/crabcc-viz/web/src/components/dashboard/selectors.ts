// Pure derivation helpers for the dashboard home tiles.
//
// Why factored: the orchestrator should compose; every tile-data
// transform that's worth a unit test lives here. Same pattern as
// `components/activity/store.ts` and `components/agents/store.ts`.

import type { ActivityHit, AgentSummary, TelemetryEvent } from "../../api";

/// One bucket of the activity sparkline. `count` is events that landed
/// in the bucket; `bucketStart` is the unix-second the bucket opened.
export interface SparkBucket {
  bucketStart: number;
  count: number;
}

/**
 * Bucket `items` into N equal-width windows ending at `now` and reaching
 * back `windowSec` seconds. Returns oldest → newest so a CSS flex row
 * renders chronologically.
 *
 * `now` is unix-seconds; items use `.ts` in unix-seconds too.
 */
export function activitySparkline(
  items: ActivityHit[],
  now: number,
  windowSec: number,
  buckets: number,
): SparkBucket[] {
  const out: SparkBucket[] = [];
  const width = Math.max(1, Math.floor(windowSec / buckets));
  const start = now - windowSec;
  for (let i = 0; i < buckets; i++) {
    out.push({ bucketStart: start + i * width, count: 0 });
  }
  for (const it of items) {
    if (it.ts < start || it.ts > now) continue;
    const ix = Math.min(buckets - 1, Math.floor((it.ts - start) / width));
    if (ix >= 0) out[ix]!.count += 1;
  }
  return out;
}

/** Total events in the most recent 60s of `items`. */
export function eventsPerMinute(items: ActivityHit[], now: number): number {
  const cutoff = now - 60;
  let n = 0;
  for (const it of items) if (it.ts >= cutoff) n += 1;
  return n;
}

/** Running agents from the SSE feed. */
export function runningAgents(agents: AgentSummary[]): AgentSummary[] {
  return agents.filter((a) => a.status === "running");
}

/** Top N telemetry events by recency (descending ts). */
export function topRecentEvents(events: TelemetryEvent[], n: number): TelemetryEvent[] {
  return [...events].sort((a, b) => b.ts - a.ts).slice(0, n);
}

/// One row in the level-breakdown pill strip on the logs page.
export interface LevelCount {
  level: string;
  count: number;
}

const LEVELS = ["TRACE", "DEBUG", "INFO", "WARN", "ERROR"] as const;

/** Counts per-level so the logs view can render its KPI pill row. */
export function levelBreakdown(events: TelemetryEvent[]): LevelCount[] {
  const counts = new Map<string, number>();
  for (const e of events) counts.set(e.level, (counts.get(e.level) ?? 0) + 1);
  return LEVELS.map((level) => ({ level, count: counts.get(level) ?? 0 }));
}

/** Unique target names in `events`, sorted by frequency descending. */
export function topTargets(events: TelemetryEvent[], n: number): string[] {
  const counts = new Map<string, number>();
  for (const e of events) counts.set(e.target, (counts.get(e.target) ?? 0) + 1);
  return [...counts.entries()]
    .sort((a, b) => b[1] - a[1])
    .slice(0, n)
    .map(([t]) => t);
}

/** Format a relative second-count to a short human label (12s, 4m, 2h). */
export function fmtAge(deltaSec: number): string {
  const dt = Math.max(0, Math.floor(deltaSec));
  if (dt < 60) return `${dt}s`;
  if (dt < 3600) return `${Math.floor(dt / 60)}m`;
  if (dt < 86400) return `${Math.floor(dt / 3600)}h`;
  return `${Math.floor(dt / 86400)}d`;
}
