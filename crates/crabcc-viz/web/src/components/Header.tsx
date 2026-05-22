import { memo } from "react";
import {
  BarChart2,
  BookOpen,
  ChevronRight,
  Circle,
  FileText,
  GitPullRequest,
  LayoutDashboard,
  RefreshCw,
  Server,
  Settings,
  Zap,
  type LucideIcon,
} from "lucide-react";
import type { Route } from "../router";
import { Icon } from "./icons";
import { cn } from "../lib/cn";
import { Button } from "./ui/button";

type Props = {
  repo: string;
  root: string;
  version: string;
  live: boolean;
  route: Route;
  mobile?: boolean;
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
  { href: "#/prs", label: "prs", match: "prs", icon: GitPullRequest },
  { href: "#/analytics", label: "health", match: "analytics", icon: BarChart2 },
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
  mobile = false,
  onReindex,
  onRandomQuery,
  onSettings,
}: Props) {
  return (
    <header
      className={cn(
        "flex items-center gap-3.5 px-3.5 py-2",
        "border-b border-border bg-card",
      )}
    >
      <span className="font-bold tracking-wider text-primary shrink-0">
        crabcc · live
      </span>
      <span
        className={cn(
          "inline-flex items-center gap-1.5 shrink-0",
          live ? "text-success" : "text-inactive",
        )}
        title={live ? "SSE connected" : "SSE disconnected"}
      >
        <Icon
          of={Circle}
          size={10}
          className="block"
          fill={live ? "currentColor" : "none"}
          aria-hidden="true"
        />
        <span className="max-[480px]:hidden">{live ? "live" : "offline"}</span>
      </span>
      {/* Desktop nav — hidden on mobile (bottom nav takes over) */}
      {!mobile && (
        <nav
          className="flex items-center gap-0.5 ml-1"
          aria-label="Primary"
        >
          {NAV.map((n) => {
            const isActive = route === n.match;
            return (
              <a
                key={n.match}
                href={n.href}
                className={cn(
                  "px-2.5 py-1 rounded text-xs font-semibold tracking-wider",
                  "no-underline border border-transparent",
                  "max-[900px]:px-1.5 max-[900px]:text-[11px]",
                  isActive
                    ? "text-primary bg-background border-border"
                    : "text-muted hover:text-foreground hover:bg-background hover:border-border",
                  "focus-visible:outline-2 focus-visible:outline-primary",
                  "focus-visible:outline-offset-2",
                )}
                aria-current={isActive ? "page" : undefined}
                tabIndex={0}
              >
                <Icon
                  of={n.icon}
                  size={12}
                  className="opacity-80 mr-1 -translate-y-px"
                  aria-hidden="true"
                />
                {n.label}
              </a>
            );
          })}
        </nav>
      )}
      <span
        className={cn(
          "flex-1 text-muted text-xs",
          "max-[1100px]:text-[11px] max-[1100px]:max-w-[200px]",
          "max-[1100px]:overflow-hidden max-[1100px]:text-ellipsis",
          "max-[1100px]:whitespace-nowrap",
          mobile ? "hidden" : "max-[800px]:hidden",
        )}
      >
        <b className="text-foreground font-semibold">{repo}</b> · {root} · v
        {version}
      </span>
      <span className="flex gap-1.5">
        <Button
          variant="outline"
          size="icon"
          onClick={onReindex}
          title="Run `crabcc index` against the server's PWD"
          aria-label="Re-index"
        >
          <Icon of={RefreshCw} />
        </Button>
        <Button
          variant="outline"
          size="icon"
          onClick={onRandomQuery}
          title="Run a random crabcc query against this repo"
          aria-label="Random query"
        >
          <Icon of={Zap} />
        </Button>
        {onSettings && (
          <Button
            variant="outline"
            size="icon"
            onClick={onSettings}
            title="Dashboard settings"
            aria-label="Open settings"
          >
            <Icon of={Settings} />
          </Button>
        )}
        {/* asChild lets the Button render as an <a> while keeping
            the outline-variant styling — same anchor behaviour as
            before, just via the new component surface. */}
        <Button asChild variant="outline" size="icon">
          <a
            href="/graph"
            title="Open the interactive call-graph viewer"
            aria-label="Interactive graph"
          >
            <Icon of={ChevronRight} size={12} />
          </a>
        </Button>
      </span>
    </header>
  );
});
