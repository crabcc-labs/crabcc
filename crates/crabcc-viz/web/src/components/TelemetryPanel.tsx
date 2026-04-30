// TelemetryPanel — issues #90 + #86 dashboard surface.
//
// Two sections:
//   1. OTLP health pill (issue #86) — shows green/red rotel status.
//      Set OTEL_EXPORTER_OTLP_ENDPOINT on the server to enable.
//   2. KPI event stream (issue #90) — events from .crabcc/telemetry.jsonl.
//
// Polling lives in App.tsx; this component is purely presentational.

import type { OtlpHealth, TelemetryEvent, TelemetrySource } from "../api";

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

// ── OTLP health pill ──────────────────────────────────────────────────────────

function OtlpPill({ health }: { health: OtlpHealth | null }) {
  if (!health) {
    return (
      <span className="otlp-pill otlp-unknown" title="Checking OTLP…">
        ● OTLP …
      </span>
    );
  }
  if (!health.endpoint) {
    return (
      <span
        className="otlp-pill otlp-disabled"
        title="Set OTEL_EXPORTER_OTLP_ENDPOINT on the server to enable"
      >
        ○ OTLP disabled
      </span>
    );
  }
  return health.reachable ? (
    <span
      className="otlp-pill otlp-ok"
      title={`rotel reachable at ${health.endpoint}`}
    >
      ● OTLP {shortHost(health.endpoint)}
    </span>
  ) : (
    <span
      className="otlp-pill otlp-err"
      title={`${health.error ?? "unreachable"} — ${health.endpoint}\nRun: task telemetry-rotel`}
    >
      ● OTLP unreachable
    </span>
  );
}

function shortHost(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

// ── main panel ────────────────────────────────────────────────────────────────

function fmtInterval(ms: number): string {
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  return `${Math.floor(s / 60)}m`;
}

export function TelemetryPanel({
  events,
  source,
  otlpHealth,
  otlpPollMs,
  now,
}: {
  events: TelemetryEvent[];
  source: TelemetrySource | null;
  otlpHealth: OtlpHealth | null;
  otlpPollMs?: number;
  now: number;
}) {
  const ordered = [...events].sort((a, b) => b.ts - a.ts);

  return (
    <div className="telemetry-panel">
      {/* ── OTLP health row ── */}
      <div className="telemetry-otlp-row">
        <OtlpPill health={otlpHealth} />
        {otlpHealth?.reachable && (
          <span className="otlp-hint dim">
            {" "}
            · spans → rotel · not stored in any DB
            {otlpPollMs !== undefined && (
              <> · probes every {fmtInterval(otlpPollMs)}</>
            )}
          </span>
        )}
        {otlpHealth && !otlpHealth.reachable && !otlpHealth.endpoint && (
          <span className="otlp-hint dim">
            {" "}
            ·{" "}
            <code>
              export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318
            </code>
          </span>
        )}
        {otlpHealth && !otlpHealth.reachable && otlpHealth.endpoint && (
          <span className="otlp-hint dim"> · run: task telemetry-rotel</span>
        )}
      </div>

      {/* ── jsonl event stream ── */}
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
