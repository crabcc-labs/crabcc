// `<SystemView />` — system info + control surfaces at #/system.
//
// What ships:
//   - bootstrap card: repo, root, version, since-page-load uptime
//   - services grid (full ServicesPanel)
//   - agents block (full AgentsPanel)
//   - ollama key card (existing OllamaKeyPanel)
//   - debug section (DebugInfo grid, non-floating)
//   - OpenAPI spec link

import { memo, useEffect, useState } from "react";
import {
  Bug,
  Circle,
  ExternalLink,
  Key,
  Server,
  ShieldAlert,
  Workflow,
} from "lucide-react";
import type { AgentSummary } from "../../api";
import { logMount, logUnmount } from "../../lifecycle";
import { useNow } from "../../useNow";
import { AgentsPanel } from "../AgentsPanel";
import { Icon } from "../icons";
import { NetlogPanel } from "../NetlogPanel";
import { OllamaKeyPanel } from "../OllamaKeyPanel";
import { ServicesPanel } from "../ServicesPanel";
import { fmtAge } from "../dashboard/selectors";

export interface SystemDebugInfo {
  sseConnected: boolean;
  sseUrl: string;
  activityCount: number;
  agentCount: number;
  telemetryCount: number;
  telemetryCursor: number;
  telemetryPath: string;
  telemetryExists: boolean;
}

interface Props {
  agents: AgentSummary[];
  bootstrap: { repo: string; root: string; version: string } | null;
  debug: SystemDebugInfo;
}

export const SystemView = memo(function SystemView({ agents, bootstrap, debug }: Props) {
  const now = useNow();
  // Browser-tab uptime; we don't have a real server-boot timestamp from
  // /api/bootstrap so the fallback (mount time) is the honest answer.
  const [mountedAt] = useState(() => Math.floor(Date.now() / 1000));

  useEffect(() => {
    logMount("SystemView");
    return () => logUnmount("SystemView");
  }, []);

  return (
    <main className="system-view">
      {/* ── overview card ───────────────────────────────────────── */}
      <section className="system-card">
        <h3 className="system-card-title">
          <Icon of={Server} size={14} className="system-card-ico" aria-hidden="true" />
          overview
        </h3>
        <dl className="system-dl">
          <dt>repo</dt>
          <dd><code>{bootstrap?.repo ?? "?"}</code></dd>
          <dt>root</dt>
          <dd><code>{bootstrap?.root ?? "?"}</code></dd>
          <dt>version</dt>
          <dd><code>{bootstrap?.version ?? "?"}</code></dd>
          <dt>since page load</dt>
          <dd>{fmtAge(now - mountedAt)}</dd>
          <dt>OpenAPI</dt>
          <dd>
            <a href="/api/openapi.yaml" target="_blank" rel="noreferrer" className="system-link">
              openapi.yaml <Icon of={ExternalLink} size={11} aria-hidden="true" />
            </a>
          </dd>
        </dl>
      </section>

      {/* ── services ────────────────────────────────────────────── */}
      <section className="system-card system-card-wide">
        <h3 className="system-card-title">
          <Icon of={Server} size={14} className="system-card-ico" aria-hidden="true" />
          services
        </h3>
        <ServicesPanel />
      </section>

      {/* ── egress / netlog (#160) ──────────────────────────────── */}
      <section className="system-card system-card-wide">
        <h3 className="system-card-title">
          <Icon of={ShieldAlert} size={14} className="system-card-ico" aria-hidden="true" />
          egress (netlog)
        </h3>
        <NetlogPanel />
      </section>

      {/* ── agents (4 tabs) ─────────────────────────────────────── */}
      <section className="system-card system-card-wide">
        <h3 className="system-card-title">
          <Icon of={Workflow} size={14} className="system-card-ico" aria-hidden="true" />
          agents
        </h3>
        <AgentsPanel agents={agents} />
      </section>

      {/* ── ollama key ──────────────────────────────────────────── */}
      <section className="system-card">
        <h3 className="system-card-title">
          <Icon of={Key} size={14} className="system-card-ico" aria-hidden="true" />
          ollama api key
        </h3>
        <OllamaKeyPanel />
      </section>

      {/* ── debug pane (non-floating) ───────────────────────────── */}
      <section className="system-card system-card-wide">
        <h3 className="system-card-title">
          <Icon of={Bug} size={14} className="system-card-ico" aria-hidden="true" />
          debug
        </h3>
        <table className="system-debug">
          <tbody>
            <tr>
              <th>SSE</th>
              <td>
                <span
                  className="system-dot"
                  style={{ color: debug.sseConnected ? "#4d7" : "#888" }}
                >
                  <Icon of={Circle} size={9} fill="currentColor" aria-hidden="true" />
                </span>{" "}
                {debug.sseConnected ? "connected" : "disconnected"} ·{" "}
                <code>{debug.sseUrl}</code>
              </td>
            </tr>
            <tr>
              <th>activity</th>
              <td>{debug.activityCount} hits</td>
            </tr>
            <tr>
              <th>agents</th>
              <td>{debug.agentCount} known</td>
            </tr>
            <tr>
              <th>telemetry</th>
              <td>
                {debug.telemetryCount} events · cursor=
                <code>{debug.telemetryCursor}</code>
              </td>
            </tr>
            <tr>
              <th>tel. file</th>
              <td>
                {debug.telemetryExists ? "OK" : "MISSING"} —{" "}
                <code>{debug.telemetryPath || "?"}</code>
              </td>
            </tr>
          </tbody>
        </table>
      </section>
    </main>
  );
});
