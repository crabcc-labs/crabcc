import { memo, useEffect, useState } from "react";
import { api, type AgentKillRow } from "../api";

// Surfaces rows from agent_kill_events — what `crabcc agent-guard`
// has SIGTERM/SIGKILL'd (zombies + stuck runs > idle threshold) over
// the last 100 incidents. Refreshes every 10s.
export const AgentKillsPanel = memo(function AgentKillsPanel() {
  const [rows, setRows] = useState<AgentKillRow[]>([]);
  const [db, setDb] = useState<string>("");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    const load = () => {
      api
        .agentKills()
        .then((r) => {
          if (!alive) return;
          setRows(r.rows);
          setDb(r.db);
          setError(null);
        })
        .catch((e) => alive && setError(String(e)));
    };
    load();
    const t = window.setInterval(load, 10_000);
    return () => {
      alive = false;
      window.clearInterval(t);
    };
  }, []);

  if (error) {
    return <div className="empty">kills unavailable: {error}</div>;
  }
  if (rows.length === 0) {
    return <div className="empty">no kill events recorded</div>;
  }
  return (
    <div className="scroll">
      <div className="profiles-dir">
        <code>{db}</code>
      </div>
      {rows.map((r) => (
        <div key={`${r.run_id}-${r.killed_at}`} className="kill-row">
          <div className="kill-head">
            <span className={`kill-reason ${r.reason}`}>{r.reason}</span>
            <span className="kill-runid">{r.run_id.slice(0, 10)}…</span>
            {r.pid != null && (
              <span className="kill-pid">pid={r.pid}</span>
            )}
            <span className="kill-ts">
              {new Date(r.killed_at * 1000).toLocaleTimeString()}
            </span>
          </div>
          {r.detail && <div className="kill-detail">{r.detail}</div>}
        </div>
      ))}
    </div>
  );
});
