// Inline detail card for an expanded agent — full id, pid, started/exit
// timestamps, exit code, and the streaming log tail.
//
// The "kill" button is best-effort: the backend doesn't currently expose
// POST /api/agents/{id}/kill (issue tracked downstream). For now we log
// the user-action breadcrumb and surface a "not wired" hint so the
// affordance is discoverable but honest about its current state.

import { memo } from "react";
import { X } from "lucide-react";
import { logUserAction } from "../../lifecycle";
import { Icon } from "../icons";
import type { AgentSummary } from "./types";
import { uptimeLabel } from "./store";
import { useAgentLog } from "./useAgentLog";

interface Props {
  agent: AgentSummary;
  pinned: boolean;
  now: number;
  onTogglePin(): void;
  onClose(): void;
}

export const AgentDetail = memo(function AgentDetail({
  agent,
  pinned,
  now,
  onTogglePin,
  onClose,
}: Props) {
  const { body, loading } = useAgentLog(agent.id, true);

  const startedHuman =
    agent.started_ts !== undefined
      ? new Date(agent.started_ts * 1000).toLocaleString()
      : "—";

  return (
    <div className="agents-detail">
      <div className="agents-detail-head">
        <span className={`agents-pill agents-pill-${agent.status}`}>
          {agent.status}
        </span>
        <span className="agents-detail-id" title={agent.id}>
          {agent.id}
        </span>
        <button
          type="button"
          className={"agents-pin" + (pinned ? " on" : "")}
          onClick={onTogglePin}
          title={pinned ? "Unpin (stay open after exit)" : "Pin"}
          aria-pressed={pinned}
        >
          {pinned ? "★" : "☆"}
        </button>
        <button
          type="button"
          className="agents-close"
          onClick={onClose}
          title="Collapse (Esc)"
          aria-label="Collapse"
        >
          <Icon of={X} size={12} />
        </button>
      </div>
      <dl className="agents-detail-grid">
        <dt>pid</dt>
        <dd>{agent.pid ?? "—"}</dd>
        <dt>model</dt>
        <dd>{agent.model ?? "—"}</dd>
        <dt>started</dt>
        <dd>{startedHuman}</dd>
        <dt>uptime</dt>
        <dd>{uptimeLabel(agent, now)}</dd>
        {agent.status === "exited" ? (
          <>
            <dt>exit code</dt>
            <dd>{agent.exit_code ?? "—"}</dd>
          </>
        ) : null}
      </dl>
      {agent.prompt_preview ? (
        <div className="agents-detail-prompt">{agent.prompt_preview}</div>
      ) : null}
      {agent.status === "running" ? (
        <div className="agents-detail-actions">
          <button
            type="button"
            className="agents-kill"
            title="Kill not yet wired — POST /api/agents/{id}/kill TBD"
            onClick={() => {
              // The kill endpoint isn't implemented yet; record the
              // intent in the lifecycle log so the breadcrumb is
              // visible to anyone watching the dev console.
              logUserAction(`agent kill requested ${agent.id} (no-op: endpoint TBD)`);
            }}
          >
            kill
          </button>
          <span className="agents-kill-note">
            (kill endpoint not yet wired)
          </span>
        </div>
      ) : null}
      <div className="agents-detail-logwrap">
        <div className="agents-detail-loglabel">log tail</div>
        <pre className="agents-log">{body || (loading ? "(loading…)" : "(no output)")}</pre>
      </div>
    </div>
  );
});
