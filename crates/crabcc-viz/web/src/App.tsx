import { useState } from "react";
import { Header } from "./components/Header";
import { ActivityPanel } from "./components/ActivityPanel";
import { AgentsPanel } from "./components/AgentsPanel";
import { ReindexDialog } from "./components/ReindexDialog";
import { usePolling } from "./usePolling";
import { useEventStream } from "./useEventStream";
import { api, type ActivityHit, type AgentSummary } from "./api";

export function App() {
  const [reindexOpen, setReindexOpen] = useState(false);
  const [activity, setActivity] = useState<ActivityHit[]>([]);
  const [agents, setAgents] = useState<AgentSummary[]>([]);
  const bootstrap = usePolling(api.bootstrap, 0);

  // Single SSE stream replaces three polling loops. The dashboard's
  // "live" indicator binds to the connection state — green when the
  // EventSource is open, grey on disconnect/backoff.
  const { connected } = useEventStream("/api/events", {
    activity: (p) => {
      const data = p as { items?: ActivityHit[] } | null;
      setActivity(data?.items ?? []);
    },
    agents: (p) => {
      const data = p as { agents?: AgentSummary[] } | null;
      setAgents(data?.agents ?? []);
    },
  });

  return (
    <div className="layout">
      <Header
        repo={bootstrap.data?.repo ?? "?"}
        root={bootstrap.data?.root ?? "?"}
        version={bootstrap.data?.version ?? "?"}
        live={connected}
        onReindex={() => setReindexOpen(true)}
        onRandomQuery={() => api.randomQuery().catch(() => {})}
      />
      <main>
        <section className="col">
          <h2>
            tool calls <span className="count">{activity.length}</span>
          </h2>
          <ActivityPanel items={activity} />
        </section>
        <section className="col stage">
          <div className="placeholder">
            relations graph — phase 2 of #17
            <small>
              (see <code>web/DESIGN.md</code>)
            </small>
          </div>
        </section>
        <section className="col">
          <h2>
            agents <span className="count">{agents.length}</span>
          </h2>
          <AgentsPanel agents={agents} />
        </section>
      </main>
      {reindexOpen && <ReindexDialog onClose={() => setReindexOpen(false)} />}
    </div>
  );
}
