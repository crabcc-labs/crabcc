// Top-overlay controls: search input + density slider. Both are
// pure DOM — overlaying the canvas via absolute positioning. The
// search debounces as the user types; submitting (Enter) triggers
// the parent to filter or fetch a fresh `/api/graph?root=…` snapshot.

import { useEffect, useId, useRef, useState } from "react";

const LIMITS = [5, 10, 20, 40, 80];

interface Props {
  limit: number;
  onLimitChange: (n: number) => void;
  searchValue: string;
  onSearchChange: (v: string) => void;
  onSearchSubmit: () => void;
  hint?: string;
}

export function Controls({
  limit,
  onLimitChange,
  searchValue,
  onSearchChange,
  onSearchSubmit,
  hint,
}: Props) {
  const inputRef = useRef<HTMLInputElement | null>(null);
  const [local, setLocal] = useState(searchValue);
  const limitId = useId();

  // Mirror parent → local on programmatic clears (Esc handler).
  useEffect(() => setLocal(searchValue), [searchValue]);

  return (
    <div className="graph-controls">
      <form
        className="graph-search"
        onSubmit={(e) => {
          e.preventDefault();
          onSearchChange(local);
          onSearchSubmit();
        }}
      >
        <input
          ref={inputRef}
          type="text"
          placeholder="search symbol… (enter)"
          value={local}
          onChange={(e) => setLocal(e.target.value)}
          spellCheck={false}
          aria-label="Search graph"
        />
        {local && (
          <button
            type="button"
            className="graph-search-clear"
            onClick={() => {
              setLocal("");
              onSearchChange("");
            }}
            aria-label="Clear search"
            title="Clear"
          >×</button>
        )}
      </form>
      <label className="graph-slider" htmlFor={limitId}>
        density
        <input
          id={limitId}
          type="range"
          min={0}
          max={LIMITS.length - 1}
          step={1}
          value={Math.max(0, LIMITS.indexOf(limit))}
          onChange={(e) => {
            const idx = Number(e.target.value);
            const next = LIMITS[idx];
            if (next !== undefined) onLimitChange(next);
          }}
        />
        <span className="graph-slider-val">{limit}</span>
      </label>
      {hint && <span className="graph-controls-hint">{hint}</span>}
    </div>
  );
}
