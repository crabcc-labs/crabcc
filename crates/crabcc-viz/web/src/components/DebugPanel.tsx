// DebugPanel — show what the dashboard is doing right now.
//
// Issue #90 follow-up — surface the dashboard's *own* runtime state so
// users can tell at a glance whether SSE is connected, when each panel
// last refreshed, how many events have been seen, and which API URLs
// the frontend is hitting. No backend round-trip; everything here is
// state already in the App.

export type DebugInfo = {
  sseConnected: boolean;
  sseUrl: string;
  activityCount: number;
  agentCount: number;
  telemetryCount: number;
  telemetryCursor: number;
  telemetryPath: string;
  telemetryExists: boolean;
  lastTelemetryPollMs: number; // unix ms
  bootstrapRoot: string;
  bootstrapRepo: string;
  bootstrapVersion: string;
  rendersSinceMount: number;
};

function fmtTime(ms: number): string {
  if (!ms) return "never";
  const dt = Math.max(0, Date.now() - ms);
  if (dt < 1000) return "just now";
  if (dt < 60_000) return `${Math.floor(dt / 1000)}s ago`;
  if (dt < 3_600_000) return `${Math.floor(dt / 60_000)}m ago`;
  return `${Math.floor(dt / 3_600_000)}h ago`;
}

export function DebugPanel({ info }: { info: DebugInfo }) {
  return (
    <details className="debug-panel" open={false}>
      <summary>
        <span
          className="debug-dot"
          style={{ background: info.sseConnected ? "#4d7" : "#888" }}
        />
        debug: current view
      </summary>
      <table className="debug-table">
        <tbody>
          <tr>
            <th>SSE</th>
            <td>
              {info.sseConnected ? "connected" : "disconnected"} —{" "}
              <code>{info.sseUrl}</code>
            </td>
          </tr>
          <tr>
            <th>repo</th>
            <td>
              <code>{info.bootstrapRepo}</code> @ <code>{info.bootstrapRoot}</code>
            </td>
          </tr>
          <tr>
            <th>version</th>
            <td>
              <code>{info.bootstrapVersion}</code>
            </td>
          </tr>
          <tr>
            <th>activity</th>
            <td>{info.activityCount} hits</td>
          </tr>
          <tr>
            <th>agents</th>
            <td>{info.agentCount} known</td>
          </tr>
          <tr>
            <th>telemetry</th>
            <td>
              {info.telemetryCount} events · cursor={info.telemetryCursor} ·
              last poll {fmtTime(info.lastTelemetryPollMs)}
            </td>
          </tr>
          <tr>
            <th>tel. file</th>
            <td>
              {info.telemetryExists ? "OK" : "MISSING"} —{" "}
              <code>{info.telemetryPath || "?"}</code>
            </td>
          </tr>
          <tr>
            <th>renders</th>
            <td>{info.rendersSinceMount} since mount</td>
          </tr>
        </tbody>
      </table>
    </details>
  );
}
