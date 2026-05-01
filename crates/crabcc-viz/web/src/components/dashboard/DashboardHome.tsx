// `<DashboardHome />` — the new info-dense home view.
//
// Goal: 1440x900 fits everything without scrolling. We pack ~9 tiles
// into a CSS-grid (grid-template-areas), and let smaller viewports
// collapse via @media. Heavy interaction lives at the dedicated routes
// (#/logs, #/system, #/knowledge); the tiles here are read-only
// previews with an "open ›" hand-off.
//
// Module conventions (matching components/{activity,agents,graph,knowledge}/):
//   - hooks for data + derivation
//   - pure selectors in `./selectors.ts`
//   - small leaf components co-located here
//   - this file is a slim orchestrator

import { Suspense, lazy, memo, useEffect } from "react";
import type { ActivityHit, AgentSummary, OtlpHealth, TelemetryEvent } from "../../api";
import { logMount, logUnmount } from "../../lifecycle";
import { useNow } from "../../useNow";
import { DashTile } from "./DashTile";
import { Sparkline } from "./Sparkline";
import {
  activitySparkline,
  eventsPerMinute,
  fmtAge,
  runningAgents,
  topRecentEvents,
} from "./selectors";
import { useMemoryRecent } from "./useMemoryRecent";
import { useServicesSummary } from "./useServicesSummary";

// Lazy-load the relations graph so the home view's first paint doesn't
// pull in d3-force. Same pattern App.tsx used originally.
const RelationsGraph = lazy(() =>
  import("../RelationsGraph").then((m) => ({ default: m.RelationsGraph })),
);

interface Props {
  connected: boolean;
  /** Unix-ms timestamp of the most recent SSE flip to "connected". */
  liveSince: number;
  activity: ActivityHit[];
  agents: AgentSummary[];
  telEvents: TelemetryEvent[];
  otlp: OtlpHealth | null;
  bootstrap: { repo: string; root: string; version: string } | null;
}

const SPARK_WINDOW_SEC = 600;
const SPARK_BUCKETS = 30;

export const DashboardHome = memo(function DashboardHome({
  connected,
  liveSince,
  activity,
  agents,
  telEvents,
  otlp,
  bootstrap,
}: Props) {
  useEffect(() => {
    logMount("DashboardHome");
    return () => logUnmount("DashboardHome");
  }, []);

  const now = useNow();
  const memory = useMemoryRecent(5);
  const services = useServicesSummary();

  const running = runningAgents(agents);
  const epm = eventsPerMinute(activity, now);
  const spark = activitySparkline(activity, now, SPARK_WINDOW_SEC, SPARK_BUCKETS);
  const recentEvents = topRecentEvents(telEvents, 10);
  const recentActivity = activity.slice(0, 20);

  const servicesUp = services.report?.services.filter((s) => s.reachable).length ?? 0;
  const servicesTotal = services.report?.services.length ?? 0;

  const liveAgeSec = liveSince > 0 ? Math.max(0, Math.floor((Date.now() - liveSince) / 1000)) : 0;

  return (
    <div className="dash-grid">
      {/* ── KPI strip ───────────────────────────────────────────── */}
      <DashTile title="live" compact area="kpi-live">
        <div className="dash-kpi">
          <span className={`dash-kpi-dot${connected ? " on" : ""}`} aria-hidden="true" />
          <span className="dash-kpi-big">{connected ? "online" : "offline"}</span>
          <span className="dash-kpi-sub">
            {connected
              ? liveAgeSec > 0
                ? `${fmtAge(liveAgeSec)} connected`
                : "just connected"
              : "no SSE"}
          </span>
        </div>
      </DashTile>

      <DashTile
        title="agents"
        compact
        area="kpi-agents"
        meta={
          <span className="dash-pill">
            {running.length}/{agents.length}
          </span>
        }
      >
        <div className="dash-kpi-list">
          {running.length === 0 ? (
            <span className="dash-kpi-sub">no running agents</span>
          ) : (
            running.slice(0, 3).map((a) => (
              <span key={a.id} className="dash-kpi-chip" title={a.id}>
                {a.id.slice(0, 8)}
              </span>
            ))
          )}
          {running.length > 3 && (
            <span className="dash-kpi-sub">+{running.length - 3} more</span>
          )}
        </div>
      </DashTile>

      <DashTile
        title="activity"
        compact
        area="kpi-activity"
        openHref="#/logs"
        openLabel="open logs"
        meta={<span className="dash-pill">{epm}/min</span>}
      >
        <Sparkline buckets={spark} height={28} />
        <div className="dash-kpi-sub">last 10 minutes · {activity.length} total</div>
      </DashTile>

      <DashTile
        title="services"
        compact
        area="kpi-services"
        openHref="#/system"
        openLabel="open system"
        meta={
          <span className="dash-pill">
            {servicesUp}/{servicesTotal}
          </span>
        }
      >
        <div className="dash-kpi-pills">
          {services.report?.services.map((s) => (
            <span
              key={s.name}
              className={`dash-svc-pill ${s.reachable ? "ok" : "down"}`}
              title={`${s.name} · ${s.url} · ${s.reachable ? `${s.latency_ms}ms` : s.error ?? "down"}`}
            >
              {s.name}
            </span>
          )) ?? <span className="dash-kpi-sub">probing…</span>}
        </div>
      </DashTile>

      {/* ── relations graph (large) ─────────────────────────────── */}
      <DashTile title="relations graph" area="graph" openHref="/graph" openLabel="interactive">
        <div className="dash-graph-host">
          <Suspense
            fallback={
              <div className="placeholder graph-placeholder">
                <span className="graph-spinner" /> loading relations graph…
              </div>
            }
          >
            <RelationsGraph limit={20} />
          </Suspense>
        </div>
      </DashTile>

      {/* ── recent telemetry events ─────────────────────────────── */}
      <DashTile
        title="recent events"
        area="events"
        openHref="#/logs"
        openLabel="all logs"
        meta={<span className="dash-pill">{telEvents.length}</span>}
      >
        {recentEvents.length === 0 ? (
          <div className="dash-empty">no telemetry yet</div>
        ) : (
          <ul className="dash-event-list">
            {recentEvents.map((e, i) => {
              const msg =
                typeof e.fields["message"] === "string" ? e.fields["message"] : "";
              return (
                <li key={`${e.ts}-${i}`} className="dash-event-row">
                  <span className={`dash-level dash-level-${e.level}`}>{e.level[0]}</span>
                  <span className="dash-event-target">
                    {e.target.replace(/^crabcc_/, "")}
                  </span>
                  <span className="dash-event-msg">{msg}</span>
                  <span className="dash-event-age">{fmtAge(now - e.ts)}</span>
                </li>
              );
            })}
          </ul>
        )}
      </DashTile>

      {/* ── activity stream condensed ───────────────────────────── */}
      <DashTile
        title="tool calls"
        area="activity"
        openHref="#/logs"
        openLabel="full stream"
        meta={<span className="dash-pill">{activity.length}</span>}
      >
        {recentActivity.length === 0 ? (
          <div className="dash-empty">waiting for agent queries…</div>
        ) : (
          <ul className="dash-activity-list">
            {recentActivity.map((h, i) => (
              <li key={`${h.ts}-${i}`} className="dash-activity-row">
                <span className="dash-activity-op">{h.op}</span>
                <span className="dash-activity-q" title={h.query}>{h.query}</span>
                <span className="dash-activity-c">{h.count}</span>
                <span className="dash-activity-age">{fmtAge(now - h.ts)}</span>
              </li>
            ))}
          </ul>
        )}
      </DashTile>

      {/* ── system status (compact) ─────────────────────────────── */}
      <DashTile
        title="system status"
        area="system"
        openHref="#/system"
        openLabel="details"
      >
        <div className="dash-svc-grid">
          {services.report?.services.map((s) => (
            <div
              key={s.name}
              className={`dash-svc-cell ${s.reachable ? "ok" : "down"}`}
              title={s.url}
            >
              <span className="dash-svc-dot" />
              <span className="dash-svc-name">{s.name}</span>
              {s.reachable && <span className="dash-svc-lat">{s.latency_ms}ms</span>}
            </div>
          )) ?? <div className="dash-empty">probing…</div>}
        </div>
      </DashTile>

      {/* ── memory drawers (last 5) ─────────────────────────────── */}
      <DashTile
        title="memory drawers"
        area="memory"
        openHref="#/knowledge"
        openLabel="knowledge graph"
        meta={memory.present ? <span className="dash-pill">{memory.drawers.length}</span> : null}
      >
        {!memory.present ? (
          <div className="dash-empty">
            no drawer db — run <code>crabcc memory init</code>
          </div>
        ) : memory.drawers.length === 0 ? (
          <div className="dash-empty">no recent drawers</div>
        ) : (
          <ul className="dash-memory-list">
            {memory.drawers.map((d) => (
              <li key={d.id} className="dash-memory-row">
                <span className="dash-memory-wing">{d.wing}</span>
                <span className="dash-memory-source" title={d.source_id}>
                  {d.source_id}
                </span>
                <span className="dash-memory-age">{fmtAge(now - d.created_at)}</span>
              </li>
            ))}
          </ul>
        )}
      </DashTile>

      {/* ── meta: otlp + version + uptime ───────────────────────── */}
      <DashTile title="health" area="health">
        <dl className="dash-health">
          <dt>OTLP</dt>
          <dd className={otlp?.reachable ? "ok" : otlp?.endpoint ? "warn" : "muted"}>
            {!otlp
              ? "checking…"
              : !otlp.endpoint
                ? "disabled"
                : otlp.reachable
                  ? `reachable · ${shortHost(otlp.endpoint)}`
                  : "unreachable"}
          </dd>
          <dt>version</dt>
          <dd>
            <code>{bootstrap?.version ?? "?"}</code>
          </dd>
          <dt>repo</dt>
          <dd>
            <code>{bootstrap?.repo ?? "?"}</code>
          </dd>
          <dt>events</dt>
          <dd>{telEvents.length} captured · {activity.length} tool calls</dd>
        </dl>
      </DashTile>
    </div>
  );
});

function shortHost(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}
