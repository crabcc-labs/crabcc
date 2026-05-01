// Selection side-panel. Pops in when the user clicks a node, shows
// degree counts and direction-aware "expand callers/callees" buttons.
//
// `expanding` reflects whether a fetch is in flight for this exact
// (id, dir) pair so the button can disable + show a spinner without
// us needing a global request-state map.

import { X } from "lucide-react";
import { Icon } from "../icons";
import type { SimNode } from "./types";
import { colorFor } from "./GraphCanvas";

interface Props {
  node: SimNode;
  inDeg: number;
  outDeg: number;
  expanding: { callers: boolean; callees: boolean };
  onClose: () => void;
  onExpand: (dir: "callers" | "callees") => void;
}

export function NodeInfo({ node, inDeg, outDeg, expanding, onClose, onExpand }: Props) {
  return (
    <div className="graph-info">
      <div className="graph-info-head">
        <span className="graph-info-dot" style={{ background: colorFor(node) }} />
        <code title={node.id}>{node.id}</code>
        <button
          className="graph-info-close"
          onClick={onClose}
          aria-label="Close panel"
          title="Close (Esc)"
        ><Icon of={X} size={12} /></button>
      </div>
      <dl className="graph-info-grid">
        <dt>depth</dt><dd>{node.depth}</dd>
        <dt>seed</dt><dd>{node.isSeed ? "yes" : "no"}</dd>
        <dt>in-degree</dt><dd>{inDeg}</dd>
        <dt>out-degree</dt><dd>{outDeg}</dd>
      </dl>
      <div className="graph-info-actions">
        <button
          onClick={() => onExpand("callers")}
          disabled={expanding.callers || node.expandedCallers}
          title="Load 2 hops of callers"
        >
          {expanding.callers ? "loading…" : node.expandedCallers ? "callers loaded" : "expand callers"}
        </button>
        <button
          onClick={() => onExpand("callees")}
          disabled={expanding.callees || node.expandedCallees}
          title="Load 2 hops of callees"
        >
          {expanding.callees ? "loading…" : node.expandedCallees ? "callees loaded" : "expand callees"}
        </button>
      </div>
    </div>
  );
}
