// One kill event row. The `reason` (zombie / stuck / manual) drives the
// pill colour via the existing `.kill-reason.*` rules in styles.css —
// we re-use them under the new agents-* prefix as `.agents-kill-reason`.

import { memo } from "react";
import type { AgentKillRow as KillEvent } from "./types";

interface Props {
  kill: KillEvent;
  selected: boolean;
  expanded: boolean;
  onPick(): void;
}

export const KillRow = memo(function KillRow({
  kill,
  selected,
  expanded,
  onPick,
}: Props) {
  const cls =
    "agents-row kill" +
    (selected ? " selected" : "") +
    (expanded ? " expanded" : "");
  return (
    <button type="button" className={cls} onClick={onPick}>
      <span
        className={`agents-kill-reason ${slug(kill.reason)}`}
        title={kill.reason}
      >
        {kill.reason}
      </span>
      <span className="agents-kill-runid" title={kill.run_id}>
        {kill.run_id.slice(0, 10)}…
      </span>
      {kill.pid !== null ? (
        <span className="agents-kill-pid">pid {kill.pid}</span>
      ) : null}
      <span className="agents-kill-ts">
        {new Date(kill.killed_at * 1000).toLocaleTimeString()}
      </span>
    </button>
  );
});

/// Slug a free-form reason string for use as a CSS class — keeps the
/// existing `.zombie / .stuck / .manual` colour rules viable for any
/// new reason values the backend introduces.
function slug(s: string): string {
  return s.toLowerCase().replace(/[^a-z0-9]+/g, "-");
}
