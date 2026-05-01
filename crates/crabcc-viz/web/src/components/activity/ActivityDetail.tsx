// Inline expanded-detail card for the currently-selected hit. Shows
// fields that don't fit in the row (full query, agent, source,
// duration), plus copy + pin actions.

import { memo, useCallback } from "react";
import type { ActivityHit } from "./types";

interface Props {
  hit: ActivityHit;
  pinned: boolean;
  onTogglePin(hit: ActivityHit): void;
  onClose(): void;
}

export const ActivityDetail = memo(function ActivityDetail({
  hit,
  pinned,
  onTogglePin,
  onClose,
}: Props) {
  const onCopy = useCallback(() => {
    // navigator.clipboard may be missing in non-secure contexts; degrade
    // silently rather than crashing the panel.
    try {
      void navigator.clipboard?.writeText(hit.query);
    } catch {
      // ignore
    }
  }, [hit.query]);

  const agent = (hit as { agent?: string }).agent;
  const dur = (hit as { dur_ms?: number }).dur_ms;

  return (
    <div className="activity-detail" role="dialog" aria-label="Activity detail">
      <div className="activity-detail-head">
        <span className="op">{hit.op}</span>
        <span className="grow" />
        <button
          type="button"
          className={"activity-pin" + (pinned ? " on" : "")}
          onClick={() => onTogglePin(hit)}
          title={pinned ? "Unpin" : "Pin"}
        >
          {pinned ? "★" : "☆"}
        </button>
        <button type="button" className="activity-copy" onClick={onCopy} title="Copy query">
          copy
        </button>
        <button type="button" className="activity-close" onClick={onClose} title="Close detail">
          ×
        </button>
      </div>
      <div className="activity-detail-body">
        <pre className="query">{hit.query}</pre>
        <dl>
          {hit.count > 1 ? (
            <>
              <dt>count</dt>
              <dd>{hit.count}</dd>
            </>
          ) : null}
          {hit.source ? (
            <>
              <dt>source</dt>
              <dd>{hit.source}</dd>
            </>
          ) : null}
          {agent ? (
            <>
              <dt>agent</dt>
              <dd>{agent}</dd>
            </>
          ) : null}
          {typeof dur === "number" ? (
            <>
              <dt>duration</dt>
              <dd>{dur} ms</dd>
            </>
          ) : null}
          <dt>ts</dt>
          <dd>{new Date(hit.ts * 1000).toLocaleTimeString()}</dd>
        </dl>
      </div>
    </div>
  );
});
