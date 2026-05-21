import { useState } from "react";
import { usePolling } from "../../usePolling";
import { api } from "../../api";
import { cn } from "../../lib/cn";
import { BarChart2, RefreshCw, Skull } from "lucide-react";
import { HotspotsTable } from "./HotspotsTable";
import { DeadCodeView } from "./DeadCodeView";

type Tab = "hotspots" | "deadcode";

export function AnalyticsView() {
  const [tab, setTab] = useState<Tab>("hotspots");

  const hotspots = usePolling(
    () => api.analyticsHotspots(50),
    0, // on-demand — expensive, don't re-poll
    [],
    { source: "/api/analytics/hotspots" },
  );

  const deadcode = usePolling(
    () => api.analyticsDeadcode(100),
    0,
    [],
    { source: "/api/analytics/deadcode" },
  );

  const hs = hotspots.data?.hotspots ?? [];
  const dc = deadcode.data?.dead_code ?? [];
  const headSha = hotspots.data?.head_sha ?? deadcode.data?.head_sha ?? "";
  const computedAt = hotspots.data?.computed_at ?? 0;

  return (
    <div className="flex flex-col gap-4 p-4 max-w-5xl mx-auto">
      {/* Header */}
      <div className="flex items-center gap-3 flex-wrap">
        <h1 className="text-lg font-bold text-foreground flex items-center gap-2">
          <BarChart2 size={20} className="text-primary" />
          Code Health
        </h1>
        {headSha && (
          <span className="text-xs text-muted bg-background border border-border rounded px-2 py-0.5 font-mono">
            {headSha}
          </span>
        )}
        {computedAt > 0 && (
          <span className="text-xs text-muted ml-auto">
            computed {relativeTime(computedAt)}
          </span>
        )}
      </div>

      {/* Stats strip */}
      <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
        <StatCard
          label="Hot files"
          value={hs.length}
          sub="by commit count"
          loading={hotspots.loading}
        />
        <StatCard
          label="Top churn"
          value={hs[0]?.commits ?? 0}
          sub={hs[0] ? shortPath(hs[0].file) : "—"}
          loading={hotspots.loading}
        />
        <StatCard
          label="Dead code"
          value={dc.length}
          sub="unreachable symbols"
          loading={deadcode.loading}
          warn={dc.length > 20}
        />
        <StatCard
          label="Commits scanned"
          value={hotspots.data?.total_commits_scanned ?? 0}
          sub={`${hotspots.data?.total_files_seen ?? 0} files`}
          loading={hotspots.loading}
        />
      </div>

      {/* Tabs */}
      <div className="flex gap-1 border-b border-border">
        <TabButton
          active={tab === "hotspots"}
          onClick={() => setTab("hotspots")}
          icon={<BarChart2 size={13} />}
          label="Hotspots"
        />
        <TabButton
          active={tab === "deadcode"}
          onClick={() => setTab("deadcode")}
          icon={<Skull size={13} />}
          label={`Dead code${dc.length ? ` (${dc.length})` : ""}`}
        />
      </div>

      {/* Tab content */}
      {tab === "hotspots" && (
        <>
          {hotspots.loading && hs.length === 0 && (
            <LoadingRow label="Scanning git log…" />
          )}
          {hs.length > 0 && <HotspotsTable hotspots={hs} />}
          {!hotspots.loading && hs.length === 0 && (
            <p className="text-muted text-sm text-center py-6">
              No git history found. Make sure you're running inside a git repo.
            </p>
          )}
        </>
      )}

      {tab === "deadcode" && (
        <>
          {deadcode.loading && dc.length === 0 && (
            <LoadingRow label="Scanning symbol index…" />
          )}
          <DeadCodeView symbols={dc} />
        </>
      )}
    </div>
  );
}

function StatCard({
  label,
  value,
  sub,
  loading,
  warn,
}: {
  label: string;
  value: number;
  sub: string;
  loading?: boolean;
  warn?: boolean;
}) {
  return (
    <div className="rounded-lg border border-border bg-card p-3">
      <div className="text-xs text-muted">{label}</div>
      {loading ? (
        <div className="h-6 w-12 bg-border rounded animate-pulse mt-1" />
      ) : (
        <div
          className={cn(
            "text-xl font-bold mt-0.5",
            warn ? "text-[#f59e0b]" : "text-foreground",
          )}
        >
          {value.toLocaleString()}
        </div>
      )}
      <div className="text-[10px] text-muted mt-0.5 truncate" title={sub}>
        {sub}
      </div>
    </div>
  );
}

function TabButton({
  active,
  onClick,
  icon,
  label,
}: {
  active: boolean;
  onClick: () => void;
  icon: React.ReactNode;
  label: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        "flex items-center gap-1.5 px-3 py-2 text-xs font-semibold border-b-2 -mb-px",
        active
          ? "border-primary text-primary"
          : "border-transparent text-muted hover:text-foreground",
      )}
    >
      {icon}
      {label}
    </button>
  );
}

function LoadingRow({ label }: { label: string }) {
  return (
    <div className="flex items-center gap-2 justify-center py-8 text-muted text-sm">
      <RefreshCw size={14} className="animate-spin" />
      {label}
    </div>
  );
}

function shortPath(p: string): string {
  const parts = p.split("/");
  return parts.slice(-2).join("/");
}

function relativeTime(secs: number): string {
  const diff = Math.floor(Date.now() / 1000) - secs;
  if (diff < 60) return "just now";
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
  return `${Math.floor(diff / 3600)}h ago`;
}
