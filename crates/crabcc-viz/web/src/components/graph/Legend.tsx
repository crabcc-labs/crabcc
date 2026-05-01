// Color/encoding legend. Pure DOM overlay — no SVG, no canvas. Sits
// in the bottom-left corner with click-toggle to collapse so it
// doesn't compete with the side panel for screen real-estate.

import { useState } from "react";

const ROWS = [
  { color: "#ff2a6d", label: "seed" },
  { color: "#00f0ff", label: "depth 1" },
  { color: "#7a5cff", label: "depth 2" },
  { color: "#ffd166", label: "depth 3" },
  { color: "#9bff8f", label: "depth 4" },
];

interface Props {
  nodes: number;
  edges: number;
}

export function Legend({ nodes, edges }: Props) {
  const [open, setOpen] = useState(true);
  return (
    <div className={`graph-legend ${open ? "" : "collapsed"}`}>
      <button
        className="graph-legend-head"
        onClick={() => setOpen((v) => !v)}
        title="Toggle legend"
      >
        legend {open ? "−" : "+"}
      </button>
      {open && (
        <>
          <ul>
            {ROWS.map((r) => (
              <li key={r.label}>
                <span className="dot" style={{ background: r.color }} />
                {r.label}
              </li>
            ))}
            <li>
              <span className="line dim" />
              edge
            </li>
            <li>
              <span className="line hot" />
              edge (pinned)
            </li>
          </ul>
          <small>{nodes} nodes · {edges} edges</small>
        </>
      )}
    </div>
  );
}
