import { useEffect, useRef, useState } from "react";
import { Header } from "./components/Header";
import { ActivityPanel } from "./components/ActivityPanel";
import { AgentsPanel } from "./components/AgentsPanel";
import { ReindexDialog } from "./components/ReindexDialog";
import { TelemetryPanel } from "./components/TelemetryPanel";
import { DebugPanel } from "./components/DebugPanel";
import { usePolling } from "./usePolling";
import { useEventStream } from "./useEventStream";
import {
  api,
  type ActivityHit,
  type AgentSummary,
  type TelemetryEvent,
  type TelemetrySource,
} from "./api";

export function App() {
  const [reindexOpen, setReindexOpen] = useState(false);
  const [activity, setActivity] = useState<ActivityHit[]>([]);
  const [agents, setAgents] = useState<AgentSummary[]>([]);
  const [now, setNow] = useState(() => Math.floor(Date.now() / 1000));
  const bootstrap = usePolling(api.bootstrap, 0);

  // Telemetry — issue #90 dashboard surface. Polls /api/telemetry every
  // 3 s. The cursor returned by the server is the max ts seen; we pass
  // it as `since` on the next poll to bound the response size to deltas.
  const telemetry = usePolling(() => api.telemetry(0, 100), 3000);
  const telEvents: TelemetryEvent[] = telemetry.data?.events ?? [];
  const telSource: TelemetrySource | null = telemetry.data?.source ?? null;

  // Tick the wall clock once per second so the relative-age timestamps
  // (`12s ago`) re-render without re-fetching.
  useEffect(() => {
    const id = setInterval(
      () => setNow(Math.floor(Date.now() / 1000)),
      1000,
    );
    return () => clearInterval(id);
  }, []);

  // Single SSE stream replaces three polling loops. The dashboard's
  // "live" indicator binds to the connection state — green when the
  // EventSource is open, grey on disconnect/backoff.
  const { connected } = useEventStream("/api/events", {
    activity: (p) => {
      const data = p as { items?: ActivityHit[] } | null;
      setActivity(data?.items ?? []);
    },
    agents: (p) => {
      const data = p as { agents?: AgentSummary[] } | null;
      setAgents(data?.agents ?? []);
    },
  });

  // Render counter — strictly diagnostic; surfaced in the debug pane.
  const renderCount = useRef(0);
  renderCount.current += 1;

  return (
    <div className="layout">
      <Header
        repo={bootstrap.data?.repo ?? "?"}
        root={bootstrap.data?.root ?? "?"}
        version={bootstrap.data?.version ?? "?"}
        live={connected}
        onReindex={() => setReindexOpen(true)}
        onRandomQuery={() => api.randomQuery().catch(() => {})}
      />
      <main>
        <section className="col">
          <h2>
            tool calls <span className="count">{activity.length}</span>
          </h2>
          <ActivityPanel items={activity} />
        </section>
        <section className="col stage">
          <div className="placeholder">
            relations graph — phase 2 of #17
            <small>
              (see <code>web/DESIGN.md</code>)
            </small>
          </div>
        </section>
        <section className="col">
          <h2>
            agents <span className="count">{agents.length}</span>
          </h2>
          <AgentsPanel agents={agents} />
          <h2 style={{ marginTop: "1.2em" }}>
            telemetry <span className="count">{telEvents.length}</span>
          </h2>
          <TelemetryPanel
            events={telEvents}
            source={telSource}
            now={now}
          />
        </section>
      </main>
      <DebugPanel
        info={{
          sseConnected: connected,
          sseUrl: "/api/events",
          activityCount: activity.length,
          agentCount: agents.length,
          telemetryCount: telEvents.length,
          telemetryCursor: telemetry.data?.cursor ?? 0,
          telemetryPath: telSource?.path ?? "",
          telemetryExists: telSource?.exists ?? false,
          lastTelemetryPollMs: telemetry.data ? Date.now() : 0,
          bootstrapRoot: bootstrap.data?.root ?? "?",
          bootstrapRepo: bootstrap.data?.repo ?? "?",
          bootstrapVersion: bootstrap.data?.version ?? "?",
          rendersSinceMount: renderCount.current,
        }}
      />
      {reindexOpen && <ReindexDialog onClose={() => setReindexOpen(false)} />}
    </div>
  );
}
