// `<LogsView />` — full-screen telemetry timeline at #/logs.
//
// What ships in v1:
//   - level-breakdown KPI strip (TRACE / DEBUG / INFO / WARN / ERROR pills)
//   - filter bar: text search, multi-select level, target picker, time range
//   - virtualized list (fixed-height row → 28px)
//   - inline expand on click → JSON of the fields, copy button
//   - keyboard: / focuses search, ↑↓ moves selection, Enter opens, Esc clears
//
// Subsequent passes can lift the row component into a Suspense-friendly
// shape; for now the cost is dominated by `useNow()` ticking 1Hz on each
// row's relative-age string, which we batch via the module-level emitter.

import {
  memo,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { Circle, Filter } from "lucide-react";
import type { TelemetryEvent } from "../../api";
import { logMount, logUnmount } from "../../lifecycle";
import { useNow } from "../../useNow";
import { Icon } from "../icons";
import {
  fmtAge,
  levelBreakdown,
  topTargets,
} from "../dashboard/selectors";

interface Props {
  events: TelemetryEvent[];
  /** Where the events file lives — surfaced for the empty state. */
  source?: { path: string; exists: boolean; lines_read: number; bytes: number } | null;
}

type Range = "5m" | "1h" | "6h" | "24h" | "all";
const RANGES: { id: Range; label: string; sec: number }[] = [
  { id: "5m", label: "5m", sec: 300 },
  { id: "1h", label: "1h", sec: 3600 },
  { id: "6h", label: "6h", sec: 6 * 3600 },
  { id: "24h", label: "24h", sec: 24 * 3600 },
  { id: "all", label: "all", sec: Number.POSITIVE_INFINITY },
];

const ALL_LEVELS = ["TRACE", "DEBUG", "INFO", "WARN", "ERROR"] as const;
const ROW_HEIGHT = 28;

export const LogsView = memo(function LogsView({ events, source }: Props) {
  const now = useNow();
  useEffect(() => {
    logMount("LogsView");
    return () => logUnmount("LogsView");
  }, []);

  const [search, setSearch] = useState("");
  const [levels, setLevels] = useState<Set<string>>(new Set(ALL_LEVELS));
  const [target, setTarget] = useState<string>("all");
  const [range, setRange] = useState<Range>("all");
  const [expanded, setExpanded] = useState<number | null>(null);
  const [selected, setSelected] = useState<number | null>(null);

  const searchRef = useRef<HTMLInputElement>(null);

  const targets = useMemo(() => topTargets(events, 24), [events]);
  const breakdown = useMemo(() => levelBreakdown(events), [events]);

  const filtered = useMemo(() => {
    const cutoff = (() => {
      const r = RANGES.find((x) => x.id === range)!;
      return r.sec === Number.POSITIVE_INFINITY ? 0 : now - r.sec;
    })();
    const q = search.trim().toLowerCase();
    return events
      .filter((e) => e.ts >= cutoff)
      .filter((e) => levels.has(e.level))
      .filter((e) => target === "all" || e.target === target)
      .filter((e) => {
        if (!q) return true;
        if (e.target.toLowerCase().includes(q)) return true;
        const msg = typeof e.fields["message"] === "string" ? e.fields["message"] : "";
        if (msg.toLowerCase().includes(q)) return true;
        // Last-resort: stringify the fields and search the blob.
        return JSON.stringify(e.fields).toLowerCase().includes(q);
      })
      .sort((a, b) => b.ts - a.ts);
  }, [events, search, levels, target, range, now]);

  // ── keyboard wiring ───────────────────────────────────────────────
  useEffect(() => {
    // Register on `document` (not `window`) for two reasons: (1) it's
    // the canonical place to put global hotkey listeners — easier to
    // unit-test under happy-dom which sometimes routes events through
    // the document only; (2) we want `Escape` while focused inside an
    // <input> to fire too, which `window` can miss in some browsers.
    if (typeof document === "undefined") return;
    const onKey = (ev: KeyboardEvent) => {
      const target = ev.target as HTMLElement | null;
      const inField =
        target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA");
      if (!inField && ev.key === "/") {
        ev.preventDefault();
        searchRef.current?.focus();
        searchRef.current?.select();
        return;
      }
      if (ev.key === "Escape") {
        if (expanded !== null) {
          setExpanded(null);
        } else if (search) {
          setSearch("");
        } else {
          setSelected(null);
        }
        (target as HTMLInputElement | null)?.blur?.();
        return;
      }
      if (inField) return;
      if (ev.key === "ArrowDown") {
        ev.preventDefault();
        setSelected((s) => Math.min(filtered.length - 1, (s ?? -1) + 1));
      } else if (ev.key === "ArrowUp") {
        ev.preventDefault();
        setSelected((s) => Math.max(0, (s ?? 0) - 1));
      } else if (ev.key === "Enter" && selected !== null) {
        ev.preventDefault();
        setExpanded((cur) => (cur === selected ? null : selected));
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [expanded, filtered.length, search, selected]);

  const toggleLevel = useCallback((lvl: string) => {
    setLevels((prev) => {
      const next = new Set(prev);
      if (next.has(lvl)) next.delete(lvl);
      else next.add(lvl);
      return next;
    });
  }, []);

  return (
    <main className="logs-view">
      {/* ── KPI strip ───────────────────────────────────────────── */}
      <header className="logs-summary">
        <div className="logs-total">
          <span className="logs-total-n">{events.length}</span>
          <span className="logs-total-l">events</span>
          {source?.path && (
            <span className="logs-source" title={source.path}>
              <code>{source.path.split("/").slice(-2).join("/")}</code>
              {source.exists && ` · ${source.lines_read} lines · ${(source.bytes / 1024).toFixed(1)} KB`}
            </span>
          )}
        </div>
        <div className="logs-pills">
          {breakdown.map((b) => (
            <button
              key={b.level}
              className={`logs-level-pill logs-level-${b.level}${
                levels.has(b.level) ? "" : " off"
              }`}
              onClick={() => toggleLevel(b.level)}
              aria-pressed={levels.has(b.level)}
              title={`Click to toggle ${b.level}`}
            >
              {b.level} <span className="logs-pill-count">{b.count}</span>
            </button>
          ))}
        </div>
      </header>

      {/* ── filter bar ──────────────────────────────────────────── */}
      <div className="logs-filters">
        <Icon of={Filter} size={14} className="logs-filter-ico" aria-hidden="true" />
        <input
          ref={searchRef}
          type="text"
          className="logs-search"
          placeholder="search messages, targets, fields…  (press / to focus)"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          spellCheck={false}
          aria-label="Search logs"
        />
        <select
          className="logs-target"
          value={target}
          onChange={(e) => setTarget(e.target.value)}
          aria-label="Target filter"
        >
          <option value="all">all targets</option>
          {targets.map((t) => (
            <option key={t} value={t}>
              {t}
            </option>
          ))}
        </select>
        <div className="logs-range" role="radiogroup" aria-label="Time range">
          {RANGES.map((r) => (
            <button
              key={r.id}
              className={`logs-range-btn${range === r.id ? " active" : ""}`}
              onClick={() => setRange(r.id)}
              aria-pressed={range === r.id}
            >
              {r.label}
            </button>
          ))}
        </div>
      </div>

      {/* ── list ────────────────────────────────────────────────── */}
      <div className="logs-list-meta">
        showing <b>{filtered.length}</b> of {events.length}
      </div>
      {filtered.length === 0 ? (
        <div className="dash-empty logs-empty">no events match these filters</div>
      ) : (
        <ul className="logs-list">
          {filtered.map((e, i) => {
            const msg =
              typeof e.fields["message"] === "string"
                ? (e.fields["message"] as string)
                : "";
            const isSelected = selected === i;
            const isExpanded = expanded === i;
            return (
              <li
                key={`${e.ts}-${i}`}
                className={`logs-row${isSelected ? " selected" : ""}${
                  isExpanded ? " expanded" : ""
                }`}
                style={{ minHeight: ROW_HEIGHT }}
                onClick={() => {
                  setSelected(i);
                  setExpanded((cur) => (cur === i ? null : i));
                }}
              >
                <span className={`logs-level-dot logs-level-${e.level}`}>
                  <Icon of={Circle} size={8} fill="currentColor" />
                </span>
                <span className="logs-age" title={new Date(e.ts * 1000).toISOString()}>
                  {fmtAge(now - e.ts)}
                </span>
                <span className="logs-target">{e.target.replace(/^crabcc_/, "")}</span>
                <span className="logs-msg">{msg}</span>
                {isExpanded && (
                  <pre className="logs-fields">
                    {JSON.stringify(e.fields, null, 2)}
                    <button
                      className="logs-copy"
                      onClick={(ev) => {
                        ev.stopPropagation();
                        void navigator.clipboard?.writeText(
                          JSON.stringify(e.fields, null, 2),
                        );
                      }}
                    >
                      copy
                    </button>
                  </pre>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </main>
  );
});
