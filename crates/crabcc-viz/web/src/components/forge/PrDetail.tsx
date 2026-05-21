import { useState, lazy, Suspense } from "react";
import { usePolling } from "../../usePolling";
import { api } from "../../api";
import { navigate } from "../../router";
import { cn } from "../../lib/cn";
import { Button } from "../ui/button";
import {
  ArrowLeft,
  GitMerge,
  GitPullRequest,
  GitPullRequestClosed,
  Network,
  RefreshCw,
} from "lucide-react";
import { DiffViewer } from "./DiffViewer";
import type { PrFile } from "../../api";

const ImpactGraph = lazy(() =>
  import("./ImpactGraph").then((m) => ({ default: m.ImpactGraph })),
);

type Tab = "diff" | "impact";

type Props = {
  prNumber: number;
};

export function PrDetail({ prNumber }: Props) {
  const [tab, setTab] = useState<Tab>("diff");

  const { data, loading } = usePolling(
    () => api.forgePr(prNumber),
    0, // no polling — PR detail is static
    [prNumber],
    { source: "/api/forge/prs/{number}" },
  );

  const { data: impactData, loading: impactLoading } = usePolling(
    () => api.forgePrImpact(prNumber),
    0,
    [prNumber, tab],
    { source: "/api/forge/prs/{number}/impact" },
  );

  const pr = data?.pr;
  const files: PrFile[] = data?.files ?? [];

  if (loading && !pr) {
    return (
      <div className="flex items-center gap-2 justify-center py-16 text-muted text-sm">
        <RefreshCw size={14} className="animate-spin" />
        Loading PR #{prNumber}…
      </div>
    );
  }

  if (!pr) {
    return (
      <div className="p-4 text-destructive text-sm">
        PR #{prNumber} not found or GitHub not configured.
      </div>
    );
  }

  const stateIcon =
    pr.merged ? (
      <GitMerge size={18} className="text-[#8b5cf6]" />
    ) : pr.state === "closed" ? (
      <GitPullRequestClosed size={18} className="text-destructive" />
    ) : (
      <GitPullRequest size={18} className="text-success" />
    );

  return (
    <div className="flex flex-col gap-4 p-4 max-w-5xl mx-auto">
      {/* Back + title */}
      <div className="flex items-start gap-3">
        <Button
          variant="outline"
          size="icon"
          onClick={() => navigate("prs")}
          title="Back to PR list"
          className="shrink-0 mt-0.5"
        >
          <ArrowLeft size={14} />
        </Button>
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2 flex-wrap">
            {stateIcon}
            <h1 className="font-bold text-base text-foreground">{pr.title}</h1>
            {pr.draft && (
              <span className="text-[10px] border border-muted/40 rounded px-1 text-muted">
                draft
              </span>
            )}
          </div>
          <div className="flex items-center gap-3 mt-1 text-xs text-muted flex-wrap">
            <span>#{pr.number}</span>
            <span className="flex items-center gap-1">
              <img
                src={pr.author.avatar_url}
                alt={pr.author.login}
                className="w-4 h-4 rounded-full"
              />
              {pr.author.login}
            </span>
            <span>
              {pr.head_ref} → {pr.base_ref}
            </span>
            <span className="text-success">+{pr.additions}</span>
            <span className="text-destructive">−{pr.deletions}</span>
            <span>{pr.changed_files} files</span>
          </div>
        </div>
        <a
          href={pr.html_url}
          target="_blank"
          rel="noreferrer"
          className="shrink-0 text-xs text-muted hover:text-primary"
        >
          GitHub ↗
        </a>
      </div>

      {/* Body (collapsed by default) */}
      {pr.body && (
        <details className="text-sm text-muted border border-border rounded p-3">
          <summary className="cursor-pointer font-semibold text-foreground">
            Description
          </summary>
          <p className="mt-2 whitespace-pre-wrap leading-relaxed">{pr.body}</p>
        </details>
      )}

      {/* Tabs */}
      <div className="flex gap-1 border-b border-border">
        <TabButton
          active={tab === "diff"}
          onClick={() => setTab("diff")}
          icon={<GitPullRequest size={13} />}
          label={`Files (${files.length})`}
        />
        <TabButton
          active={tab === "impact"}
          onClick={() => setTab("impact")}
          icon={<Network size={13} />}
          label="Impact graph"
        />
      </div>

      {/* Diff tab */}
      {tab === "diff" && (
        <div className="flex flex-col gap-3">
          {pr.changed_files > files.length && (
            <div className="rounded border border-warning/40 bg-warning/5 px-3 py-2 text-xs text-warning">
              This PR has {pr.changed_files} changed files — only the first {files.length} are shown.
            </div>
          )}
          {files.map((f) => (
            <DiffViewer key={f.filename} file={f} />
          ))}
          {files.length === 0 && (
            <p className="text-muted text-sm text-center py-6">No files changed.</p>
          )}
        </div>
      )}

      {/* Impact graph tab */}
      {tab === "impact" && (
        <div>
          {impactLoading && !impactData && (
            <div className="flex items-center gap-2 justify-center py-12 text-muted text-sm">
              <RefreshCw size={14} className="animate-spin" />
              Computing blast radius…
            </div>
          )}
          {impactData && (
            <>
              {impactData.truncated && (
                <div className="rounded border border-warning/40 bg-warning/5 px-3 py-2 text-xs text-warning mb-2">
                  Graph is incomplete — file list or node count exceeded the 300-item cap.
                </div>
              )}
              <Suspense fallback={<div className="py-12 text-center text-muted text-sm">Loading graph…</div>}>
                <ImpactGraph data={impactData} />
              </Suspense>
            </>
          )}
          {!impactLoading && !impactData && (
            <p className="text-muted text-sm text-center py-6">
              Impact data unavailable — run{" "}
              <code className="text-xs">crabcc index && crabcc graph build</code>.
            </p>
          )}
        </div>
      )}
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
