import { memo, useEffect, useState } from "react";
import type { AgentSummary } from "../api";
import { AgentLogView } from "./AgentLogView";
import { logMount, logUnmount } from "../lifecycle";

export const AgentsPanel = memo(function AgentsPanel({
  agents,
}: {
  agents: AgentSummary[];
}) {
  const [openId, setOpenId] = useState<string | null>(null);

  useEffect(() => {
    logMount("AgentsPanel");
    return () => logUnmount("AgentsPanel");
  }, []);

  if (agents.length === 0) {
    return <div className="empty">No agent runs yet.</div>;
  }
  return (
    <div className="scroll">
      {agents.map((a) => (
        <div
          key={a.id}
          className={`agent${openId === a.id ? " selected" : ""}${a.status === "running" ? "" : " exited"}`}
          onClick={() => setOpenId(openId === a.id ? null : a.id)}
        >
          <div className="agent-row">
            <span className={`status ${a.status}`}>{a.status}</span>
            <span className="id">{a.id.slice(0, 8)}</span>
            <span className="prompt">{a.prompt_preview ?? "(no prompt)"}</span>
          </div>
          {openId === a.id && <AgentLogView id={a.id} />}
        </div>
      ))}
    </div>
  );
});
