import { memo } from "react";
import { CircleAlert, CircleCheck } from "lucide-react";
import { api, type NetlogResponse } from "../api";
import { usePolling } from "../usePolling";
import { Icon } from "./icons";

// Outbound-egress panel (#160 Phase 3). Polls /api/netlog every 5s: recent
// infra HTTP requests netlog recorded, and which hosts the allowlist blocked.
// An `ok=false` row is an allowlist violation — a caller (or a compromised
// dependency) trying to reach a host that isn't on the embedded allowlist.
// Reuses the ServicesPanel table styling (services-* classes).
export const NetlogPanel = memo(function NetlogPanel() {
  const { data, error } = usePolling<NetlogResponse>(
    () => api.netlog(0, 100),
    5_000,
    [],
    {
      source: "/api/netlog",
      summarize: (d) => `${d.events.length} events, ${d.violations} blocked`,
    },
  );

  if (error) {
    return <div className="empty">netlog unavailable: {String(error)}</div>;
  }
  if (!data) {
    return <div className="empty">loading…</div>;
  }
  if (data.events.length === 0) {
    return (
      <div className="empty">
        no outbound egress recorded yet — netlog appends to{" "}
        <code>~/.crabcc/netlog.jsonl</code> as infra HTTP fires.
      </div>
    );
  }

  // Newest first.
  const rows = [...data.events].reverse();
  return (
    <div className="services-panel">
      <div className="services-meta">
        <span>{data.events.length} recent</span>
        <span>
          {" · "}
          {data.violations > 0 ? (
            <strong style={{ color: "#e66" }}>{data.violations} blocked</strong>
          ) : (
            "0 blocked"
          )}
        </span>
      </div>
      <table className="services-table">
        <thead>
          <tr>
            <th>caller</th>
            <th>host</th>
            <th>port</th>
            <th>state</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((e, i) => (
            <tr
              key={`${e.ts}-${i}`}
              className={e.ok ? "service-ok" : "service-down"}
            >
              <td>{e.caller}</td>
              <td>
                <code>{e.host}</code>
              </td>
              <td>{e.port}</td>
              <td>
                {e.ok ? (
                  <span className="service-state ok">
                    <Icon of={CircleCheck} size={12} aria-hidden="true" /> allowed
                  </span>
                ) : (
                  <span
                    className="service-state down"
                    title="host not on the netlog allowlist — blocked"
                  >
                    <Icon of={CircleAlert} size={12} aria-hidden="true" /> blocked
                  </span>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
});
