import { memo } from "react";

type Props = {
  repo: string;
  root: string;
  version: string;
  live: boolean;
  onReindex: () => void;
  onRandomQuery: () => void;
};

/// Memoized so a per-tick activity poll doesn't re-render the header.
/// React.memo's default shallow-prop check is enough here — the props
/// only change on bootstrap completion or a connectivity flip.
export const Header = memo(function Header({
  repo,
  root,
  version,
  live,
  onReindex,
  onRandomQuery,
}: Props) {
  return (
    <header>
      <span className="brand">crabcc · live</span>
      <span className={`live${live ? " on" : ""}`}>
        <span className="dot" />
        <span>{live ? "live" : "offline"}</span>
      </span>
      <span className="crumb">
        <b>{repo}</b> · {root} · v{version}
      </span>
      <span className="actions">
        <button onClick={onReindex} title="Run `crabcc index` against the server's PWD">
          ↻ Re-index PWD
        </button>
        <button onClick={onRandomQuery} title="Run a random crabcc query against this repo">
          ⚡ Random query
        </button>
        <a href="/graph" title="Open the interactive call-graph viewer">
          interactive ›
        </a>
      </span>
    </header>
  );
});
