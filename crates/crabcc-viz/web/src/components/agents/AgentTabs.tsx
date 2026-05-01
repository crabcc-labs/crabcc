// Top-of-panel tab strip: live · profiles · kills · models. Each tab
// shows its source-of-truth count as a small badge.

import { memo } from "react";
import type { TabId } from "./types";
import { TAB_ORDER } from "./types";

interface Props {
  active: TabId;
  onPick(t: TabId): void;
  counts: Record<TabId, number>;
}

const LABELS: Record<TabId, string> = {
  live: "live",
  profiles: "profiles",
  kills: "kills",
  models: "models",
};

export const AgentTabs = memo(function AgentTabs({ active, onPick, counts }: Props) {
  return (
    <div className="agents-tabs" role="tablist" aria-label="Agents tabs">
      {TAB_ORDER.map((id, i) => {
        const isActive = id === active;
        return (
          <button
            key={id}
            type="button"
            role="tab"
            aria-selected={isActive}
            className={"agents-tab" + (isActive ? " active" : "")}
            onClick={() => onPick(id)}
            title={`${LABELS[id]} (${i + 1})`}
          >
            <span className="agents-tab-label">{LABELS[id]}</span>
            <span className="agents-tab-count">{counts[id]}</span>
          </button>
        );
      })}
    </div>
  );
});
