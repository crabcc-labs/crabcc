// One rendered row — either a literal hit or a collapsed group.
// Memoized so virtualization re-paints don't invalidate every row;
// only the rows whose `selected` / `pinned` state changed re-render.

import { memo } from "react";
import { relativeTime } from "./store";
import type { ActivityHit, Row } from "./types";

interface Props {
  row: Row;
  selected: boolean;
  searchText: string;
  now: number;
  onPick(row: Row): void;
}

export const ActivityRow = memo(function ActivityRow({
  row,
  selected,
  searchText,
  now,
  onPick,
}: Props) {
  if (row.kind === "header") {
    // Headers are rendered separately (see ActivityHeader); a row
    // appearing as a header here only happens when the parent slices
    // and we re-route — defensive default.
    return <div className="activity-header">{row.label}</div>;
  }

  if (row.kind === "group") {
    const cls =
      "activity-row group" +
      (row.expanded ? " expanded" : "") +
      (selected ? " selected" : "");
    return (
      <button
        type="button"
        className={cls}
        onClick={() => onPick(row)}
        aria-expanded={row.expanded}
      >
        <span className="op">{row.op}</span>
        <span className="query">
          <span className="group-count">{row.count}×</span>{" "}
          {highlight(row.lastQuery, searchText)}
        </span>
        <span className="meta">{relativeTime(row.lastTs, now)}</span>
      </button>
    );
  }

  return (
    <ActivityHitRow
      hit={row.hit}
      pinned={row.pinned}
      selected={selected}
      searchText={searchText}
      now={now}
      onPick={() => onPick(row)}
    />
  );
});

interface HitProps {
  hit: ActivityHit;
  pinned: boolean;
  selected: boolean;
  searchText: string;
  now: number;
  onPick(): void;
}

const ActivityHitRow = memo(function ActivityHitRow({
  hit,
  pinned,
  selected,
  searchText,
  now,
  onPick,
}: HitProps) {
  const cls =
    "activity-row hit" +
    (selected ? " selected" : "") +
    (pinned ? " pinned" : "");
  return (
    <button type="button" className={cls} onClick={onPick}>
      <span className="op">{hit.op}</span>
      <span className="query">{highlight(hit.query, searchText)}</span>
      <span className="meta">
        {hit.count > 1 ? <span className="count">{hit.count}</span> : null}
        <span className="age">{relativeTime(hit.ts, now)}</span>
      </span>
    </button>
  );
});

/// Highlight matching substring. Returns the raw text when search is
/// empty — avoids a span wrapper on every row.
function highlight(text: string, query: string): React.ReactNode {
  if (!query) return text;
  const lower = text.toLowerCase();
  const q = query.toLowerCase();
  const idx = lower.indexOf(q);
  if (idx < 0) return text;
  return (
    <>
      {text.slice(0, idx)}
      <mark>{text.slice(idx, idx + q.length)}</mark>
      {text.slice(idx + q.length)}
    </>
  );
}
