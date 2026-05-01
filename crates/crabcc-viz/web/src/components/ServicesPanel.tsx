import { memo, useEffect, useState } from "react";
import { CircleAlert, CircleCheck } from "lucide-react";
import { api, type ServiceStatus, type DiscoveryReport } from "../api";
import { Icon } from "./icons";
import {
  logFetchErr,
  logFetchOk,
  logMount,
  logUnmount,
} from "../lifecycle";

// Service-discovery panel (issue #143). Pulls /api/services every 15s.
// Each row: service name + resolved URL + source (env-var name vs `default`)
// + reachability state. URL source highlights which env var was honored,
// so the panel doubles as "which knob would I twiddle to change this?"
//
// Reachability is a TCP-connect probe with an 800ms timeout — same data
// the `crabcc debug-service-discovery` CLI shows. No protocol-level checks
// here (those live in `crabcc doctor stack` / `crabcc doctor jobs`).
export const ServicesPanel = memo(function ServicesPanel() {
  const [report, setReport] = useState<DiscoveryReport | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    logMount("ServicesPanel");
    let alive = true;
    const load = () => {
      api
        .services()
        .then((r) => {
          if (!alive) return;
          setReport(r);
          setError(null);
          const up = r.services.filter((s) => s.reachable).length;
          logFetchOk(
            "/api/services",
            `${r.services.length} entries (${up} up)`,
          );
        })
        .catch((e) => {
          if (!alive) return;
          setError(String(e));
          logFetchErr("/api/services", e);
        });
    };
    load();
    const t = window.setInterval(load, 15_000);
    return () => {
      alive = false;
      window.clearInterval(t);
      logUnmount("ServicesPanel");
    };
  }, []);

  if (error) {
    return <div className="empty">services unavailable: {error}</div>;
  }
  if (!report) {
    return <div className="empty">loading…</div>;
  }

  const upCount = report.services.filter((s) => s.reachable).length;

  return (
    <div className="services-panel">
      <div className="services-meta">
        <span>
          {upCount}/{report.services.length} up
        </span>
        <span> · compose: {report.compose_mode ? "yes" : "no"}</span>
        <span> · probed in {report.elapsed_ms}ms</span>
      </div>
      <table className="services-table">
        <thead>
          <tr>
            <th>service</th>
            <th>url</th>
            <th>source</th>
            <th>state</th>
          </tr>
        </thead>
        <tbody>
          {report.services.map((s) => (
            <ServiceRow key={s.name} svc={s} />
          ))}
        </tbody>
      </table>
    </div>
  );
});

function ServiceRow({ svc }: { svc: ServiceStatus }) {
  const sourceLabel =
    svc.source === "default" ? "default" : `$${svc.source}`;
  return (
    <tr className={svc.reachable ? "service-ok" : "service-down"}>
      <td>{svc.name}</td>
      <td>
        <code>{svc.url}</code>
      </td>
      <td className="service-source">{sourceLabel}</td>
      <td>
        {svc.reachable ? (
          <span className="service-state ok">
            <Icon of={CircleCheck} size={12} aria-hidden="true" /> {svc.latency_ms}ms
          </span>
        ) : (
          <span
            className="service-state down"
            title={svc.error ?? "down"}
          >
            <Icon of={CircleAlert} size={12} aria-hidden="true" />{" "}
            {(svc.error ?? "down").slice(0, 40)}
          </span>
        )}
      </td>
    </tr>
  );
}
