import { useEffect, useRef, useState, useCallback } from "react";
import { Header } from "./components/Header";
import { ActivityPanel } from "./components/ActivityPanel";
import { AgentsPanel } from "./components/AgentsPanel";
import { AgentProfilesPanel } from "./components/AgentProfilesPanel";
import { AgentKillsPanel } from "./components/AgentKillsPanel";
import { AgentModelsPanel } from "./components/AgentModelsPanel";
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

export function App() {
  const [reindexOpen, setReindexOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settings, setSettings] = useState<Settings>(loadSettings);
  const [activity, setActivity] = useState<ActivityHit[]>([]);
  const [agents, setAgents] = useState<AgentSummary[]>([]);

  // Push the live-web state into window.__crabcc__ so the Chrome
  // extension (#184) can read it via chrome.scripting.executeScript.
  useEffect(() => {
    updateDebugBridge({ activityCount: activity.length });
  }, [activity.length]);
  useEffect(() => {
    updateDebugBridge({ agentCount: agents.length });
  }, [agents.length]);
  const [now, setNow] = useState(() => Math.floor(Date.now() / 1000));
  const bootstrap = usePolling(api.bootstrap, 0, [], {
    source: "/api/bootstrap",
    summarize: (b) => `repo=${b.repo} version=${b.version}`,
  });

  // Apply new settings + reload the page so all polling intervals update.
  const applySettings = useCallback((s: Settings) => {
    setSettings(s);
    saveSettings(s);
    // Reload to re-init all usePolling instances with the new intervals.
    window.location.reload();
  }, []);

  // Telemetry — issue #90. Interval from settings (default 3 s).
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

  // Issue #86 — OTLP health probe. Interval from settings (default 30 s).
  const otlpHealth = usePolling(api.otlpHealth, settings.otlpPollMs, [], {
    source: "/api/otlp-health",
    summarize: (h) => (h.reachable ? "reachable" : "down"),
  });
  const otlpData: OtlpHealth | null = otlpHealth.data ?? null;

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
      const items = data?.items ?? [];
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
            agent profiles
          </h2>
          <AgentProfilesPanel />
          <h2 style={{ marginTop: "1.2em" }}>
            recent kills
          </h2>
          <AgentKillsPanel />
          <h2 style={{ marginTop: "1.2em" }}>
            models
          </h2>
          <AgentModelsPanel />
          <h2 style={{ marginTop: "1.2em" }}>
            services
          </h2>
          <ServicesPanel />
          <h2 style={{ marginTop: "1.2em" }}>
            ollama api key
          </h2>
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
