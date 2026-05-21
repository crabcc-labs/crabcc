import { useState } from "react";
import { usePolling } from "../../usePolling";
import { api } from "../../api";
import { navigate } from "../../router";
import { cn } from "../../lib/cn";
import {
  AlertCircle,
  GitMerge,
  GitPullRequest,
  GitPullRequestClosed,
  RefreshCw,
} from "lucide-react";
import { Button } from "../ui/button";
import type { PrSummary } from "../../api";

type StateFilter = "open" | "closed" | "all";

export function PrsView() {
  const [stateFilter, setStateFilter] = useState<StateFilter>("open");
  const [page, setPage] = useState(1);

  const { data, loading, error } = usePolling(
    () => api.forgePrs(stateFilter, page),
    30_000,
    [stateFilter, page],
    { source: "/api/forge/prs" },
  );

  const prs = data?.prs ?? [];
  const repo = data?.repo ?? "";

  return (
    <div className="flex flex-col gap-4 p-4 max-w-5xl mx-auto">
      {/* Header */}
      <div className="flex items-center gap-3 flex-wrap">
        <h1 className="text-lg font-bold text-foreground flex items-center gap-2">
          <GitPullRequest size={20} className="text-primary" />
          Pull Requests
        </h1>
        {repo && (
          <span className="text-xs text-muted bg-background border border-border rounded px-2 py-0.5">
            {repo}
          </span>
        )}
        <div className="flex gap-1 ml-auto">
          {(["open", "closed", "all"] as StateFilter[]).map((s) => (
            <button
              key={s}
              onClick={() => {
                setStateFilter(s);
                setPage(1);
              }}
              className={cn(
                "px-2.5 py-1 rounded text-xs font-semibold border",
                stateFilter === s
                  ? "bg-primary text-white border-primary"
                  : "bg-background text-muted border-border hover:text-foreground",
              )}
            >
              {s}
            </button>
          ))}
        </div>
      </div>

      {/* Config missing — check for the backend message OR any 4xx that implies
          the forge is unconfigured (getJson throws "${status} ${statusText}"). */}
      {error && (error.message?.includes("no GitHub repo") || error.message?.startsWith("400")) && (
        <div className="rounded-lg border border-destructive/30 bg-destructive/5 p-4 text-sm text-destructive flex gap-2">
          <AlertCircle size={16} className="shrink-0 mt-0.5" />
          <div>
            <p className="font-semibold">GitHub repo not configured</p>
            <p className="mt-1 text-muted">
              Set{" "}
              <code className="bg-background px-1 py-0.5 rounded text-xs">
                CRABCC_FORGE_TOKEN
              </code>{" "}
              and{" "}
              <code className="bg-background px-1 py-0.5 rounded text-xs">
                CRABCC_FORGE_REPO=owner/repo
              </code>
              , then restart <code className="text-xs">crabcc serve</code>.
            </p>
          </div>
        </div>
      )}

      {/* Loading */}
      {loading && prs.length === 0 && (
        <div className="flex items-center gap-2 text-muted text-sm py-8 justify-center">
          <RefreshCw size={14} className="animate-spin" />
          Loading pull requests…
        </div>
      )}

      {/* PR list */}
      {prs.length > 0 && (
        <div className="flex flex-col gap-2">
          {prs.map((pr) => (
            <PrCard key={pr.number} pr={pr} />
          ))}
        </div>
      )}

      {/* Empty state */}
      {!loading && !error && prs.length === 0 && (
        <p className="text-muted text-sm py-8 text-center">
          No {stateFilter} pull requests found.
        </p>
      )}

      {/* Pagination — keep controls visible while page > 1 so Prev is always
          reachable, even if the current page is empty. */}
      {((data?.total ?? 0) > 0 || page > 1) && (
        <div className="flex items-center gap-2 justify-center mt-2">
          <Button
            variant="outline"
            size="sm"
            disabled={page <= 1}
            onClick={() => setPage((p) => Math.max(1, p - 1))}
          >
            ← Prev
          </Button>
          <span className="text-xs text-muted">Page {page}</span>
          <Button
            variant="outline"
            size="sm"
            disabled={(data?.total ?? 0) < 30}
            onClick={() => setPage((p) => p + 1)}
          >
            Next →
          </Button>
        </div>
      )}
    </div>
  );
}

function PrCard({ pr }: { pr: PrSummary }) {
  const stateIcon =
    pr.merged ? (
      <GitMerge size={15} className="text-[#8b5cf6]" />
    ) : pr.state === "closed" ? (
      <GitPullRequestClosed size={15} className="text-destructive" />
    ) : (
      <GitPullRequest size={15} className="text-success" />
    );

  return (
    <button
      type="button"
      onClick={() => navigate("prs", `${pr.number}`)}
      className={cn(
        "w-full text-left rounded-lg border border-border bg-card p-3",
        "hover:border-primary/50 hover:bg-background transition-colors",
        "focus-visible:outline-2 focus-visible:outline-primary focus-visible:outline-offset-2",
        pr.draft && "opacity-70",
      )}
    >
      <div className="flex items-start gap-2">
        <span className="mt-0.5 shrink-0">{stateIcon}</span>
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2 flex-wrap">
            <span className="font-semibold text-sm text-foreground truncate">
              {pr.title}
            </span>
            {pr.draft && (
              <span className="text-[10px] border border-muted/40 rounded px-1 text-muted">
                draft
              </span>
            )}
          </div>
          <div className="flex items-center gap-3 mt-1 flex-wrap">
            <span className="text-xs text-muted">#{pr.number}</span>
            <span className="text-xs text-muted flex items-center gap-1">
              <img
                src={pr.author.avatar_url}
                alt={pr.author.login}
                className="w-4 h-4 rounded-full"
              />
              {pr.author.login}
            </span>
            <span className="text-xs text-muted">
              {pr.head_ref} → {pr.base_ref}
            </span>
            {pr.labels.slice(0, 3).map((l) => (
              <span
                key={l.name}
                className="text-[10px] rounded-full px-2 py-px border"
                style={{
                  borderColor: `#${l.color}40`,
                  color: `#${l.color}`,
                  background: `#${l.color}15`,
                }}
              >
                {l.name}
              </span>
            ))}
          </div>
        </div>
        <div className="shrink-0 text-right text-xs text-muted">
          <div className="text-success">+{pr.additions}</div>
          <div className="text-destructive">−{pr.deletions}</div>
        </div>
      </div>
    </button>
  );
}
