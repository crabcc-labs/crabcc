// DebugPanel — show what the dashboard is doing right now.
//
// Issue #90 follow-up — surface the dashboard's *own* runtime state so
// users can tell at a glance whether SSE is connected, when each panel
// last refreshed, how many events have been seen, and which API URLs
// the frontend is hitting. No backend round-trip; everything here is
// state already in the App.
//
// Track B.5 — migrated to Tailwind utilities. The legacy `.debug-*`
// rules are deleted from styles.css; this component was the only
// consumer of that family.

import { useEffect } from "react";
import { logMount, logUnmount } from "../lifecycle";
import { cn } from "../lib/cn";

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

// Tailwind class strings hoisted to module scope so the per-render
// shape stays `&'static str`-equivalent across observation ticks.
const PANEL = cn(
  "fixed bottom-2 right-2 z-50",
  "bg-card border border-border rounded-md",
  "font-mono text-[11px] leading-snug",
  "max-w-[480px]",
  "shadow-[0_4px_12px_rgba(0,0,0,0.3)]",
);

const SUMMARY = cn(
  "cursor-pointer px-2.5 py-1.5 select-none",
  "flex items-center gap-1.5",
  // Hide the default disclosure triangle; the dot is the indicator.
  "[&::-webkit-details-marker]:hidden",
  "list-none",
);

const TABLE = "border-collapse w-full m-0 px-2.5 py-1.5";
const TH = cn(
  "text-left font-semibold text-muted",
  "py-0.5 pr-2 pl-2.5",
  "align-top text-[10px] lowercase whitespace-nowrap",
);
const TD = "py-0.5 pr-2.5 break-all";
const CODE = "text-[10px]";

export function DebugPanel({ info }: { info: DebugInfo }) {
  useEffect(() => {
    logMount("DebugPanel");
    return () => logUnmount("DebugPanel");
  }, []);
  return (
    <details className={cn(PANEL, "[&[open]_summary]:border-b [&[open]_summary]:border-border")} open={false}>
      <summary className={SUMMARY}>
        <span
          className="inline-block w-[7px] h-[7px] rounded-full"
          style={{ background: info.sseConnected ? "#4d7" : "#888" }}
        />
        debug: current view
      </summary>
      <table className={TABLE}>
        <tbody>
          <tr>
            <th className={TH}>SSE</th>
            <td className={TD}>
              {info.sseConnected ? "connected" : "disconnected"} —{" "}
              <code className={CODE}>{info.sseUrl}</code>
            </td>
          </tr>
          <tr>
            <th className={TH}>repo</th>
            <td className={TD}>
              <code className={CODE}>{info.bootstrapRepo}</code> @{" "}
              <code className={CODE}>{info.bootstrapRoot}</code>
            </td>
          </tr>
          <tr>
            <th className={TH}>version</th>
            <td className={TD}>
              <code className={CODE}>{info.bootstrapVersion}</code>
            </td>
          </tr>
          <tr>
            <th className={TH}>activity</th>
            <td className={TD}>{info.activityCount} hits</td>
          </tr>
          <tr>
            <th className={TH}>agents</th>
            <td className={TD}>{info.agentCount} known</td>
          </tr>
          <tr>
            <th className={TH}>telemetry</th>
            <td className={TD}>
              {info.telemetryCount} events · cursor={info.telemetryCursor} ·
              last poll {fmtTime(info.lastTelemetryPollMs)}
            </td>
          </tr>
          <tr>
            <th className={TH}>tel. file</th>
            <td className={TD}>
              {info.telemetryExists ? "OK" : "MISSING"} —{" "}
              <code className={CODE}>{info.telemetryPath || "?"}</code>
            </td>
          </tr>
          <tr>
            <th className={TH}>renders</th>
            <td className={TD}>{info.rendersSinceMount} since mount</td>
          </tr>
        </tbody>
      </table>
    </details>
  );
}
