// One rendered agent row. Memoized so a fresh poll (which replaces
// the array reference) doesn't repaint the rows whose data didn't
// change.

import { memo } from "react";
import { uptimeLabel } from "./store";
import type { AgentSummary } from "./types";

interface Props {
  agent: AgentSummary;
  selected: boolean;
  expanded: boolean;
  now: number;
  onPick(): void;
}

export const AgentRow = memo(function AgentRow({
  agent,
  selected,
  expanded,
  now,
  onPick,
}: Props) {
  const cls =
    "agents-row" +
    (selected ? " selected" : "") +
    (expanded ? " expanded" : "") +
    (agent.status === "running" ? " running" : " exited");
  return (
    <button type="button" className={cls} onClick={onPick}>
      <span className={`agents-pill agents-pill-${agent.status}`}>
        {agent.status === "running" ? "●" : "○"} {agent.status}
      </span>
      <span className="agents-id" title={agent.id}>
        {agent.id.slice(0, 8)}
      </span>
      <span className="agents-prompt">
        {agent.prompt_preview ?? "(no prompt)"}
      </span>
      <span className="agents-meta">
        {agent.pid !== undefined ? (
          <span className="agents-pid">pid {agent.pid}</span>
        ) : null}
        <span className="agents-uptime">{uptimeLabel(agent, now)}</span>
      </span>
    </button>
  );
});
