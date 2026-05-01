// Search input + group-by toggle. Forwarded ref so the keyboard-shortcut
// handler can focus it from anywhere on the page.

import { forwardRef, memo } from "react";

interface Props {
  value: string;
  onChange(v: string): void;
  groupBy: boolean;
  onToggleGroup(): void;
  totalShown: number;
  totalAll: number;
}

export const ActivitySearch = memo(
  forwardRef<HTMLInputElement, Props>(function ActivitySearch(
    { value, onChange, groupBy, onToggleGroup, totalShown, totalAll },
    ref,
  ) {
    return (
      <div className="activity-search">
        <input
          ref={ref}
          type="search"
          placeholder="Filter… ( / to focus )"
          value={value}
          onChange={(e) => onChange(e.target.value)}
          aria-label="Filter activity"
          spellCheck={false}
          autoComplete="off"
        />
        <button
          type="button"
          className={"activity-toggle" + (groupBy ? " on" : "")}
          onClick={onToggleGroup}
          title="Group by op (g)"
          aria-pressed={groupBy}
        >
          group
        </button>
        <span className="activity-tally" title={`${totalShown} shown of ${totalAll}`}>
          {totalShown === totalAll ? totalAll : `${totalShown}/${totalAll}`}
        </span>
      </div>
    );
  }),
);
