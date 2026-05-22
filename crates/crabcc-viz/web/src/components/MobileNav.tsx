import { memo } from "react";
import {
  BarChart2,
  BookOpen,
  FileText,
  GitPullRequest,
  LayoutDashboard,
  Server,
  type LucideIcon,
} from "lucide-react";
import type { Route } from "../router";
import { cn } from "../lib/cn";

interface NavItem {
  href: string;
  label: string;
  match: Route;
  icon: LucideIcon;
}

const ITEMS: ReadonlyArray<NavItem> = [
  { href: "#/", label: "Overview", match: "dashboard", icon: LayoutDashboard },
  { href: "#/prs", label: "PRs", match: "prs", icon: GitPullRequest },
  { href: "#/analytics", label: "Health", match: "analytics", icon: BarChart2 },
  { href: "#/knowledge", label: "Memory", match: "knowledge", icon: BookOpen },
  { href: "#/logs", label: "Logs", match: "logs", icon: FileText },
  { href: "#/system", label: "System", match: "system", icon: Server },
];

type Props = {
  route: Route;
};

export const MobileNav = memo(function MobileNav({ route }: Props) {
  return (
    <nav
      className={cn(
        "fixed bottom-0 left-0 right-0 z-50",
        "flex items-stretch",
        "bg-card border-t border-border",
        "safe-area-padding-bottom",
      )}
      aria-label="Mobile navigation"
      style={{ paddingBottom: "env(safe-area-inset-bottom, 0px)" }}
    >
      {ITEMS.map((item) => {
        const active = route === item.match;
        const Ic = item.icon;
        return (
          <a
            key={item.match}
            href={item.href}
            className={cn(
              "flex flex-col items-center justify-center gap-0.5 flex-1 py-2",
              "text-[10px] font-semibold tracking-wider no-underline",
              "touch-manipulation select-none",
              active
                ? "text-primary"
                : "text-muted hover:text-foreground",
            )}
            aria-current={active ? "page" : undefined}
          >
            <Ic
              size={20}
              className={cn(active ? "text-primary" : "text-muted")}
              aria-hidden="true"
            />
            <span>{item.label}</span>
          </a>
        );
      })}
    </nav>
  );
});
