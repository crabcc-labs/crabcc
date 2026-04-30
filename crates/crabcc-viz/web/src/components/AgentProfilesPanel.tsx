import { memo, useEffect, useState } from "react";
import { api, type AgentProfileEntry } from "../api";
import {
  logFetchErr,
  logFetchOk,
  logMount,
  logUnmount,
} from "../lifecycle";

// Lists the per-crate agent profiles shipped under internal_agents/.
// Read-only for v1; future iterations add "launch this profile" CTA
// once the manager exposes a POST /api/agent/launch endpoint.
export const AgentProfilesPanel = memo(function AgentProfilesPanel() {
  const [profiles, setProfiles] = useState<AgentProfileEntry[]>([]);
  const [dir, setDir] = useState<string>("");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    logMount("AgentProfilesPanel");
    let alive = true;
    const load = () => {
      api
        .agentProfiles()
        .then((r) => {
          if (!alive) return;
          setProfiles(r.profiles);
          setDir(r.dir);
          setError(null);
          logFetchOk("/api/agent-profiles", `${r.profiles.length} profiles`);
        })
        .catch((e) => {
          if (!alive) return;
          setError(String(e));
          logFetchErr("/api/agent-profiles", e);
        });
    };
    load();
    const t = window.setInterval(load, 5_000);
    return () => {
      alive = false;
      window.clearInterval(t);
      logUnmount("AgentProfilesPanel");
    };
  }, []);

  if (error) {
    return <div className="empty">profiles unavailable: {error}</div>;
  }
  if (profiles.length === 0) {
    return (
      <div className="empty">
        no profiles in <code>{dir || "internal_agents/"}</code>
      </div>
    );
  }
  return (
    <div className="scroll">
      <div className="profiles-dir">
        <code>{dir}</code>
      </div>
      {profiles.map((p) => (
        <div key={p.id} className="profile-row">
          <div className="profile-id">{p.id}</div>
          <div className="profile-meta">
            {p.crate_ && <span>crate={p.crate_}</span>}
            {p.model && <span>model={p.model}</span>}
          </div>
          {p.description && (
            <div className="profile-desc">{p.description}</div>
          )}
        </div>
      ))}
    </div>
  );
});
