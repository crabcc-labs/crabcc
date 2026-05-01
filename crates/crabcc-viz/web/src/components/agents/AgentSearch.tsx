// Per-tab search input + optional sort selector + manual refresh.
// Forwarded ref so the keyboard "/" shortcut can focus it from
// anywhere on the page.

import { forwardRef, memo, type ReactNode } from "react";
import { RefreshCw } from "lucide-react";
import { Icon } from "../icons";

interface Props {
  value: string;
  onChange(v: string): void;
  placeholder: string;
  totalShown: number;
  totalAll: number;
  onRefresh?(): void;
  /** Optional inline controls (e.g. sort selector) shown between input and tally. */
  controls?: ReactNode;
}

export const AgentSearch = memo(
  forwardRef<HTMLInputElement, Props>(function AgentSearch(
    { value, onChange, placeholder, totalShown, totalAll, onRefresh, controls },
    ref,
  ) {
    return (
      <div className="agents-search">
        <input
          ref={ref}
          type="search"
          placeholder={placeholder}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          aria-label={placeholder}
          spellCheck={false}
          autoComplete="off"
        />
        {controls}
        <span
          className="agents-tally"
          title={`${totalShown} shown of ${totalAll}`}
        >
          {totalShown === totalAll ? totalAll : `${totalShown}/${totalAll}`}
        </span>
        {onRefresh ? (
          <button
            type="button"
            className="agents-refresh"
            onClick={onRefresh}
            title="Refresh (r)"
            aria-label="Refresh"
          >
            <Icon of={RefreshCw} size={12} />
          </button>
        ) : null}
      </div>
    );
  }),
);
