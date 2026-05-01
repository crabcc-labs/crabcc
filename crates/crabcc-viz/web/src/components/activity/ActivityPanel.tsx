// Slim orchestrator. Wires together:
//   useActivityFilter  → filter state + filtered hits
//   useActivityRows    → group + bucket + pin pipeline
//   useVirtualWindow   → fixed-row virtualization
//   useKeyboardControls→ /, Esc, ↑/↓, Enter, g
//
// Keep this file's responsibilities to *composition* — every slice of
// derivation lives in the hook layer, and every chunk of rendering is
// a small component below `components/activity/`. Mirrors the
// graph-viewer module structure.

import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { logMount, logUnmount } from "../../lifecycle";
import type { ActivityHit } from "../../api";
import { useNow } from "../../useNow";
import { ActivityDetail } from "./ActivityDetail";
import { ActivityHeader } from "./ActivityHeader";
import { ActivityRow } from "./ActivityRow";
import { ActivitySearch } from "./ActivitySearch";
import { useActivityFilter } from "./useActivityFilter";
import { useActivityRows } from "./useActivityRows";
import { useKeyboardControls } from "./useKeyboardControls";
import { useVirtualWindow } from "./useVirtualWindow";
import { pinId } from "./store";
import type { Row } from "./types";

const ROW_HEIGHT = 26;

interface Props {
  items: ActivityHit[];
}

// `now` previously came in as a prop threaded from App.tsx, which made
// App.tsx re-render every second. The `useNow` hook subscribes via a
// module-level emitter, so only this leaf re-renders on tick — App,
// RelationsGraph, etc. don't.
export const ActivityPanel = memo(function ActivityPanel({ items }: Props) {
  const now = useNow();
  useEffect(() => {
    logMount("ActivityPanel");
    return () => logUnmount("ActivityPanel");
  }, []);

  const { filter, setText, clear, filtered } = useActivityFilter(items);
  const {
    rows,
    groupBy,
    toggleGroupBy,
    toggleExpanded,
    pinnedIds,
    togglePin,
    unpinAll,
  } = useActivityRows(filtered, now);

  // Selection — index into the rows array, navigable via ↑/↓.
  const [selectedIdx, setSelectedIdx] = useState<number | null>(null);
  // Expanded-detail anchor — keyed by pinId so the detail survives the
  // row's index moving as new events arrive.
  const [detailKey, setDetailKey] = useState<string | null>(null);

  const { containerRef, window, scrollToTop, scrollToIndex, scrolledAway } =
    useVirtualWindow(rows.length, ROW_HEIGHT);

  // Tail-follow — pin the scroller to the top when the user is at top.
  // The moment they scroll down, we let go; a "↑ N new" affordance
  // shows up so they can jump back.
  const [tailFollow, setTailFollow] = useState(true);
  const [newSinceAway, setNewSinceAway] = useState(0);
  const prevHead = useRef<string | null>(null);
  const head = rows.length > 0 ? rows[0].key : null;

  useEffect(() => {
    if (head !== prevHead.current) {
      prevHead.current = head;
      if (scrolledAway) setNewSinceAway((n) => n + 1);
    }
  }, [head, scrolledAway]);

  useEffect(() => {
    // Reset the "↑ N new" counter when we're back at the top — either
    // because the user scrolled up themselves or `tailFollow` snapped
    // us there.
    if (!scrolledAway) setNewSinceAway(0);
  }, [scrolledAway]);

  useEffect(() => {
    if (tailFollow && scrolledAway) setTailFollow(false);
  }, [scrolledAway, tailFollow]);

  const onSearchRef = useRef<HTMLInputElement>(null);

  const focusSearch = useCallback(() => {
    onSearchRef.current?.focus();
    onSearchRef.current?.select();
  }, []);
  const clearOrUnpin = useCallback(() => {
    if (filter.text) {
      clear();
    } else if (pinnedIds.size > 0) {
      unpinAll();
    } else {
      setSelectedIdx(null);
      setDetailKey(null);
    }
  }, [filter.text, pinnedIds.size, clear, unpinAll]);

  const selectPrev = useCallback(() => {
    setSelectedIdx((idx) => {
      const next = nextSelectable(rows, idx, -1);
      if (next !== null) scrollToIndex(next);
      return next;
    });
  }, [rows, scrollToIndex]);
  const selectNext = useCallback(() => {
    setSelectedIdx((idx) => {
      const next = nextSelectable(rows, idx, +1);
      if (next !== null) scrollToIndex(next);
      return next;
    });
  }, [rows, scrollToIndex]);

  const openSelected = useCallback(() => {
    if (selectedIdx === null) return;
    const row = rows[selectedIdx];
    if (!row) return;
    if (row.kind === "group") toggleExpanded(row.key);
    else if (row.kind === "hit") setDetailKey(pinId(row.hit));
  }, [rows, selectedIdx, toggleExpanded]);

  useKeyboardControls(
    useMemo(
      () => ({
        focusSearch,
        clearOrUnpin,
        selectPrev,
        selectNext,
        openSelected,
        toggleGroupBy,
      }),
      [focusSearch, clearOrUnpin, selectPrev, selectNext, openSelected, toggleGroupBy],
    ),
    true,
  );

  // Click → pick. Group rows toggle their expansion; hits select + open.
  const onPick = useCallback(
    (row: Row) => {
      const idx = rows.findIndex((r) => r.key === row.key);
      if (idx >= 0) setSelectedIdx(idx);
      if (row.kind === "group") toggleExpanded(row.key);
      if (row.kind === "hit") {
        const id = pinId(row.hit);
        setDetailKey((k) => (k === id ? null : id));
      }
    },
    [rows, toggleExpanded],
  );

  // Resolve which hit (if any) is currently shown in the detail card.
  const detailHit = useMemo(() => {
    if (!detailKey) return null;
    for (const r of rows) {
      if (r.kind === "hit" && pinId(r.hit) === detailKey) return r.hit;
    }
    // Pinned hit may have scrolled off the unpinned list; fall back to source.
    for (const h of items) if (pinId(h) === detailKey) return h;
    return null;
  }, [rows, items, detailKey]);

  // ── Empty / loading paths.
  if (items.length === 0) {
    return (
      <div className="activity-panel">
        <ActivitySearch
          ref={onSearchRef}
          value={filter.text}
          onChange={setText}
          groupBy={groupBy}
          onToggleGroup={toggleGroupBy}
          totalShown={0}
          totalAll={0}
        />
        <div className="empty">Waiting for agent queries…</div>
      </div>
    );
  }

  // ── Render.
  const visible = rows.slice(window.start, window.end);
  return (
    <div className="activity-panel">
      <ActivitySearch
        ref={onSearchRef}
        value={filter.text}
        onChange={setText}
        groupBy={groupBy}
        onToggleGroup={toggleGroupBy}
        totalShown={filtered.length}
        totalAll={items.length}
      />
      <div className="activity-scroll" ref={containerRef}>
        {scrolledAway && newSinceAway > 0 ? (
          <button
            type="button"
            className="activity-tail-jump"
            onClick={() => {
              setNewSinceAway(0);
              setTailFollow(true);
              scrollToTop();
            }}
          >
            ↑ {newSinceAway} new
          </button>
        ) : null}
        <div
          className="activity-virtual"
          style={{ height: rows.length * ROW_HEIGHT }}
        >
          <div style={{ transform: `translateY(${window.padTop}px)` }}>
            {visible.map((row, i) => {
              const idx = window.start + i;
              if (row.kind === "header") {
                return <ActivityHeader key={row.key} label={row.label} />;
              }
              return (
                <ActivityRow
                  key={row.key}
                  row={row}
                  selected={idx === selectedIdx}
                  searchText={filter.text}
                  now={now}
                  onPick={onPick}
                />
              );
            })}
          </div>
        </div>
      </div>
      {detailHit ? (
        <ActivityDetail
          hit={detailHit}
          pinned={pinnedIds.has(pinId(detailHit))}
          onTogglePin={togglePin}
          onClose={() => setDetailKey(null)}
        />
      ) : null}
    </div>
  );
});

/// Find the next selectable (non-header) row from `idx` in direction
/// `dir` (+1 or -1). Wraps at the ends. Returns null if no rows exist.
function nextSelectable(
  rows: Row[],
  current: number | null,
  dir: 1 | -1,
): number | null {
  if (rows.length === 0) return null;
  let i = current === null ? (dir > 0 ? -1 : rows.length) : current;
  for (let step = 0; step < rows.length; step++) {
    i = (i + dir + rows.length) % rows.length;
    if (rows[i].kind !== "header") return i;
  }
  return null;
}
