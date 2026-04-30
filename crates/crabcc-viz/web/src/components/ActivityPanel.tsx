import { memo } from "react";
import type { ActivityHit } from "../api";

/// Renders the most-recent N tool calls. Each row is keyed on
/// `${ts}-${query}` so React's reconciler reuses DOM nodes when the
/// list grows from the top — no full subtree replacement on every
/// activity poll.
///
/// Phase 4 (#17) replaces this with a virtualized list (react-window or
/// hand-rolled tail-N + ResizeObserver) once the poll cadence justifies
/// the dependency.
export const ActivityPanel = memo(function ActivityPanel({
  items,
}: {
  items: ActivityHit[];
}) {
  if (items.length === 0) {
    return <div className="empty">Waiting for agent queries…</div>;
  }
  return (
    <div className="scroll">
      {items.map((h) => (
        <div className="hit" key={`${h.ts}-${h.op}-${h.query}`}>
          <span className="op">{h.op}</span>
          <span className="query">{h.query}</span>
          <span className="count">{h.count}</span>
        </div>
      ))}
    </div>
  );
});
