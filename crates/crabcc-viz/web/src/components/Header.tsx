import { memo } from "react";
import type { Route } from "../router";

type Props = {
  repo: string;
  root: string;
  version: string;
  live: boolean;
  route: Route;
  onReindex: () => void;
  onRandomQuery: () => void;
  onSettings?: () => void;
};

interface NavLink {
  href: string;
  label: string;
  match: Route;
}

const NAV: ReadonlyArray<NavLink> = [
  { href: "#/", label: "overview", match: "dashboard" },
  { href: "#/logs", label: "logs", match: "logs" },
  { href: "#/system", label: "system", match: "system" },
  { href: "#/knowledge", label: "knowledge", match: "knowledge" },
];

/// Memoized so a per-tick activity poll doesn't re-render the header.
/// React.memo's default shallow-prop check is enough here — the props
/// only change on bootstrap, route flip, or a connectivity flip.
export const Header = memo(function Header({
  repo,
  root,
  version,
  live,
  route,
  onReindex,
  onRandomQuery,
  onSettings,
}: Props) {
  return (
    <header>
      <span className="brand">crabcc · live</span>
      <span className={`live${live ? " on" : ""}`} title={live ? "SSE connected" : "SSE disconnected"}>
        <span className="dot" />
        <span>{live ? "live" : "offline"}</span>
      </span>
      <nav className="nav-strip" aria-label="Primary">
        {NAV.map((n) => (
          <a
            key={n.match}
            href={n.href}
            className={`nav-link${route === n.match ? " active" : ""}`}
            aria-current={route === n.match ? "page" : undefined}
            tabIndex={0}
          >
            {n.label}
          </a>
        ))}
      </nav>
      <span className="crumb">
        <b>{repo}</b> · {root} · v{version}
      </span>
      <span className="actions">
        <button
          className="icon-btn"
          onClick={onReindex}
          title="Run `crabcc index` against the server's PWD"
          aria-label="Re-index"
        >
          ↻
        </button>
        <button
          className="icon-btn"
          onClick={onRandomQuery}
          title="Run a random crabcc query against this repo"
          aria-label="Random query"
        >
          ⚡
        </button>
        {onSettings && (
          <button
            className="icon-btn"
            onClick={onSettings}
            title="Dashboard settings"
            aria-label="Open settings"
          >
            ⚙
          </button>
        )}
        <a
          className="icon-btn"
          href="/graph"
          title="Open the interactive call-graph viewer"
          aria-label="Interactive graph"
        >
          ›
        </a>
      </span>
    </header>
  );
});
