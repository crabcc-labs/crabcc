import { memo } from "react";
import {
  BookOpen,
  ChevronRight,
  Circle,
  FileText,
  LayoutDashboard,
  RefreshCw,
  Server,
  Settings,
  Zap,
  type LucideIcon,
} from "lucide-react";
import type { Route } from "../router";
import { Icon } from "./icons";

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
  icon: LucideIcon;
}

const NAV: ReadonlyArray<NavLink> = [
  { href: "#/", label: "overview", match: "dashboard", icon: LayoutDashboard },
  { href: "#/logs", label: "logs", match: "logs", icon: FileText },
  { href: "#/system", label: "system", match: "system", icon: Server },
  { href: "#/knowledge", label: "knowledge", match: "knowledge", icon: BookOpen },
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
        {/* Filled circle when connected, outline when offline — gives a
            crisper read than the prior CSS-only background-color flip. */}
        <Icon
          of={Circle}
          size={10}
          className="dot"
          fill={live ? "currentColor" : "none"}
          aria-hidden="true"
        />
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
            <Icon of={n.icon} size={12} className="nav-link-ico" aria-hidden="true" />
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
          <Icon of={RefreshCw} />
        </button>
        <button
          className="icon-btn"
          onClick={onRandomQuery}
          title="Run a random crabcc query against this repo"
          aria-label="Random query"
        >
          <Icon of={Zap} />
        </button>
        {onSettings && (
          <button
            className="icon-btn"
            onClick={onSettings}
            title="Dashboard settings"
            aria-label="Open settings"
          >
            <Icon of={Settings} />
          </button>
        )}
        <a
          className="icon-btn"
          href="/graph"
          title="Open the interactive call-graph viewer"
          aria-label="Interactive graph"
        >
          <Icon of={ChevronRight} size={12} />
        </a>
      </span>
    </header>
  );
});
