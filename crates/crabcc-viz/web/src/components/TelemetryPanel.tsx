// TelemetryPanel — issue #90 dashboard surface.
//
// Renders the tracing events produced by every crabcc invocation that
// shares the repo. Each line in `<root>/.crabcc/telemetry.jsonl` is
// one tracing event; the Rust handler parses it into a typed event
// shape and the panel groups them by KPI tag for at-a-glance throughput.
//
// What the user sees:
//   - top: a tiny meta bar (file path, lines read, bytes, exists?)
//   - main list: most recent events, newest first, with level + target
//     + the structured fields rendered as a compact key=value strip.
//   - colour-coded level dots (INFO=cyan, WARN=amber, ERROR=red).
//
// Polling lives in App.tsx via usePolling; this component is purely
// presentational.

import type { TelemetryEvent, TelemetrySource } from "../api";

const LEVEL_DOT: Record<string, string> = {
  TRACE: "#8aa",
  DEBUG: "#6cf",
  INFO: "#5dd",
  WARN: "#fb5",
  ERROR: "#f55",
};

function fmtFields(fields: Record<string, unknown>): string {
  const skip = new Set(["message"]);
  return Object.entries(fields)
    .filter(([k]) => !skip.has(k))
    .map(([k, v]) => `${k}=${typeof v === "string" ? v : JSON.stringify(v)}`)
    .join(" ");
}

function fmtAge(ts: number, now: number): string {
  const dt = Math.max(0, now - ts);
  if (dt < 60) return `${dt}s`;
  if (dt < 3600) return `${Math.floor(dt / 60)}m`;
  if (dt < 86400) return `${Math.floor(dt / 3600)}h`;
  return `${Math.floor(dt / 86400)}d`;
}

export function TelemetryPanel({
  events,
  source,
  now,
}: {
  events: TelemetryEvent[];
  source: TelemetrySource | null;
  now: number;
}) {
  // Newest first.
  const ordered = [...events].sort((a, b) => b.ts - a.ts);

  return (
    <div className="telemetry-panel">
      <div className="telemetry-meta">
        {source ? (
          source.exists ? (
            <span title={source.path}>
              <code>{source.path.split("/").slice(-2).join("/")}</code>
              {" · "}
              {source.lines_read} lines, {(source.bytes / 1024).toFixed(1)} KB
            </span>
          ) : (
            <span className="dim">
              no telemetry yet —{" "}
              <code>{source.path.split("/").slice(-2).join("/")}</code>
            </span>
          )
        ) : (
          <span className="dim">loading…</span>
        )}
      </div>
      {ordered.length === 0 ? (
        <div className="telemetry-empty">
          run any <code>crabcc graph …</code> or use the MCP server to populate
          this panel
        </div>
      ) : (
        <ul className="telemetry-list">
          {ordered.map((e, i) => {
            const msg =
              typeof e.fields.message === "string" ? e.fields.message : "";
            const fields = fmtFields(e.fields);
            return (
              <li key={`${e.ts}-${i}`} className="telemetry-row">
                <span
                  className="telemetry-dot"
                  style={{ background: LEVEL_DOT[e.level] ?? "#888" }}
                />
                <span className="telemetry-age">{fmtAge(e.ts, now)}</span>
                <span className="telemetry-target">
                  {e.target.replace(/^crabcc_/, "")}
                </span>
                {msg && <span className="telemetry-msg">{msg}</span>}
                {fields && <span className="telemetry-fields">{fields}</span>}
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
