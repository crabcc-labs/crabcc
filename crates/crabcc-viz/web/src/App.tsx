import { lazy, Suspense, useCallback, useEffect, useRef, useState } from "react";
import { Header } from "./components/Header";
import { ActivityPanel } from "./components/ActivityPanel";
import { AgentsPanel } from "./components/AgentsPanel";
import { OllamaKeyPanel } from "./components/OllamaKeyPanel";
import { ServicesPanel } from "./components/ServicesPanel";
import { ReindexDialog } from "./components/ReindexDialog";
import { TelemetryPanel } from "./components/TelemetryPanel";
import { DebugPanel } from "./components/DebugPanel";
import {
  SettingsPanel,
  loadSettings,
  saveSettings,
  type Settings,
} from "./components/SettingsPanel";
import { usePolling } from "./usePolling";
import { useEventStream } from "./useEventStream";
import { updateDebugBridge } from "./debugBridge";
import { logFetchOk, logUserAction } from "./lifecycle";
import {
  api,
  type ActivityHit,
  type AgentSummary,
  type TelemetryEvent,
  type TelemetrySource,
  type OtlpHealth,
} from "./api";

// Code-split the graph: ~150 KB of d3-force + canvas hit-testing that
// the dashboard doesn't need on its critical path. Suspense renders the
// existing placeholder styling while the chunk loads.
const RelationsGraph = lazy(() =>
  import("./components/RelationsGraph").then((m) => ({ default: m.RelationsGraph })),
);

export function App() {
  const [reindexOpen, setReindexOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settings, setSettings] = useState<Settings>(loadSettings);
  const [activity, setActivity] = useState<ActivityHit[]>([]);
  const [agents, setAgents] = useState<AgentSummary[]>([]);

  // Mirror activity / agent counts onto the debug bridge so the Chrome
  // extension (#184) can read them via chrome.scripting.executeScript.
  useEffect(() => {
    updateDebugBridge({ activityCount: activity.length });
  }, [activity.length]);
  useEffect(() => {
    updateDebugBridge({ agentCount: agents.length });
  }, [agents.length]);

  const bootstrap = usePolling(api.bootstrap, 0, [], {
    source: "/api/bootstrap",
    summarize: (b) => `repo=${b.repo} version=${b.version}`,
  });

  // Apply new settings + reload the page so all polling intervals update.
  const applySettings = useCallback((s: Settings) => {
    setSettings(s);
    saveSettings(s);
    window.location.reload();
  }, []);

  const telemetry = usePolling(
    () => api.telemetry(0, settings.telMaxEvents),
    settings.telPollMs,
    [],
    {
      source: "/api/telemetry",
      summarize: (t) => `${t.events?.length ?? 0} events`,
    },
  );
  const telEvents: TelemetryEvent[] = telemetry.data?.events ?? [];
  const telSource: TelemetrySource | null = telemetry.data?.source ?? null;

  const otlpHealth = usePolling(api.otlpHealth, settings.otlpPollMs, [], {
    source: "/api/otlp-health",
    summarize: (h) => (h.reachable ? "reachable" : "down"),
  });
  const otlpData: OtlpHealth | null = otlpHealth.data ?? null;

  // Single SSE stream replaces three polling loops. The dashboard's
  // "live" indicator binds to the connection state.
  //
  // The wire protocol uses `events` / `results`, while the OpenAPI
  // contract names them `items` / `count`. Accept either — the
  // dashboard was silently dropping real activity for a while because
  // of this mismatch.
  const { connected } = useEventStream("/api/events", {
    activity: (p) => {
      const data = p as
        | {
            items?: ActivityHit[];
            events?: { ts: number; op: string; query: string; results?: number; count?: number; source?: string }[];
          }
        | null;
      const raw = data?.items ?? data?.events ?? [];
      const items: ActivityHit[] = raw
        .map((e) => ({
          ts: e.ts,
          op: e.op,
          query: e.query,
          count:
            (e as { count?: number }).count ??
            (e as { results?: number }).results ??
            1,
          source: (e as { source?: string }).source,
        }))
        .sort((a, b) => b.ts - a.ts);
      setActivity(items);
      logFetchOk("sse:activity", `${items.length} items`);
    },
    agents: (p) => {
      const data = p as { agents?: AgentSummary[] } | null;
      const list = data?.agents ?? [];
      setAgents(list);
      const running = list.filter((a) => a.status === "running").length;
      logFetchOk("sse:agents", `${list.length} agents (${running} running)`);
    },
  });

  const renderCount = useRef(0);
  renderCount.current += 1;

  return (
    <div className="layout">
      <Header
        repo={bootstrap.data?.repo ?? "?"}
        root={bootstrap.data?.root ?? "?"}
        version={bootstrap.data?.version ?? "?"}
        live={connected}
        onReindex={() => {
          logUserAction("reindex requested");
          setReindexOpen(true);
        }}
        onRandomQuery={() => {
          logUserAction("random-query requested");
          return api.randomQuery().catch(() => {});
        }}
      />
      <main>
        <section className="col">
          <h2>
            tool calls <span className="count">{activity.length}</span>
          </h2>
          <ActivityPanel items={activity} />
        </section>
        <section className="col stage">
          <Suspense
            fallback={
              <div className="placeholder graph-placeholder">
                <span className="graph-spinner" /> loading relations graph…
              </div>
            }
          >
            <RelationsGraph limit={20} />
          </Suspense>
        </section>
        <section className="col">
          <AgentsPanel agents={agents} />
          <h2 style={{ marginTop: "1.2em" }}>services</h2>
          <ServicesPanel />
          <h2 style={{ marginTop: "1.2em" }}>ollama api key</h2>
          <OllamaKeyPanel />
          <h2 style={{ marginTop: "1.2em" }}>
            telemetry <span className="count">{telEvents.length}</span>
            <button
              className="settings-gear"
              onClick={() => setSettingsOpen(true)}
              title="Dashboard settings"
              aria-label="Open settings"
            >⚙</button>
          </h2>
          <TelemetryPanel
            events={telEvents}
            source={telSource}
            otlpHealth={otlpData}
            otlpPollMs={settings.otlpPollMs}
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
      {settingsOpen && (
        <SettingsPanel
          settings={settings}
          onChange={applySettings}
          onClose={() => setSettingsOpen(false)}
        />
      )}
    </div>
  );
}
