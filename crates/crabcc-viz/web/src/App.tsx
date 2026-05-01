// AppShell — the dashboard's top-level orchestrator. Owns:
//   - bootstrap polling (bumps on first load, cheap thereafter)
//   - the SSE connection (single source of truth for activity + agents)
//   - telemetry polling + OTLP health pings
//   - settings panel state + reindex modal
//
// Each route (`<DashboardHome />`, `<LogsView />`, `<SystemView />`,
// `<KnowledgeView />`) is a code-split chunk that consumes the data
// owned here. The shell also renders the header (with the new nav
// strip) and the modal dialogs.

import {
  Suspense,
  lazy,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import { Header } from "./components/Header";
import { ReindexDialog } from "./components/ReindexDialog";
import {
  SettingsPanel,
  loadSettings,
  saveSettings,
  type Settings,
} from "./components/SettingsPanel";
import { useEventStream } from "./useEventStream";
import { usePolling } from "./usePolling";
import { updateDebugBridge } from "./debugBridge";
import { logFetchOk, logUserAction } from "./lifecycle";
import { useRoute } from "./router";
import {
  api,
  type ActivityHit,
  type AgentSummary,
  type OtlpHealth,
  type TelemetryEvent,
  type TelemetrySource,
} from "./api";

// Each route is a lazy chunk so the dashboard's first paint doesn't
// pull in the logs / system / knowledge bundles. esbuild's `splitting`
// is wired in `esbuild.config.mjs` — even when it's off (current
// `format: iife`), `lazy()` still defers component construction.
const DashboardHome = lazy(() =>
  import("./components/dashboard").then((m) => ({ default: m.DashboardHome })),
);
const LogsView = lazy(() =>
  import("./components/logs").then((m) => ({ default: m.LogsView })),
);
const SystemView = lazy(() =>
  import("./components/system").then((m) => ({ default: m.SystemView })),
);
const KnowledgeView = lazy(() =>
  import("./components/knowledge").then((m) => ({ default: m.KnowledgeView })),
);

export function App() {
  const route = useRoute();
  const [reindexOpen, setReindexOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settings, setSettings] = useState<Settings>(loadSettings);
  const [activity, setActivity] = useState<ActivityHit[]>([]);
  const [agents, setAgents] = useState<AgentSummary[]>([]);

  // Mirror activity / agent counts onto the debug bridge so the Chrome
  // extension can read them via chrome.scripting.executeScript.
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

  // Track the last SSE connect-flip so the dashboard's "live" KPI tile
  // can show "connected for Xs". Without this, the dashboard would just
  // show "online" with no temporal anchor.
  const [liveSince, setLiveSince] = useState(0);
  const prevConnected = useRef(false);

  // Single SSE stream replaces three polling loops. The dashboard's
  // "live" indicator binds to the connection state.
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

  useEffect(() => {
    if (connected && !prevConnected.current) {
      setLiveSince(Date.now());
    }
    prevConnected.current = connected;
  }, [connected]);

  const bs = bootstrap.data
    ? {
        repo: bootstrap.data.repo,
        root: bootstrap.data.root,
        version: bootstrap.data.version,
      }
    : null;

  return (
    <div className="layout">
      <Header
        repo={bs?.repo ?? "?"}
        root={bs?.root ?? "?"}
        version={bs?.version ?? "?"}
        live={connected}
        route={route}
        onReindex={() => {
          logUserAction("reindex requested");
          setReindexOpen(true);
        }}
        onRandomQuery={() => {
          logUserAction("random-query requested");
          return api.randomQuery().catch(() => {});
        }}
        onSettings={() => setSettingsOpen(true)}
      />
      <Suspense fallback={<div className="placeholder">loading view…</div>}>
        {route === "knowledge" ? (
          <KnowledgeView />
        ) : route === "logs" ? (
          <LogsView events={telEvents} source={telSource} />
        ) : route === "system" ? (
          <SystemView
            agents={agents}
            bootstrap={bs}
            debug={{
              sseConnected: connected,
              sseUrl: "/api/events",
              activityCount: activity.length,
              agentCount: agents.length,
              telemetryCount: telEvents.length,
              telemetryCursor: telemetry.data?.cursor ?? 0,
              telemetryPath: telSource?.path ?? "",
              telemetryExists: telSource?.exists ?? false,
            }}
          />
        ) : (
          <DashboardHome
            connected={connected}
            liveSince={liveSince}
            activity={activity}
            agents={agents}
            telEvents={telEvents}
            otlp={otlpData}
            bootstrap={bs}
          />
        )}
      </Suspense>
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
