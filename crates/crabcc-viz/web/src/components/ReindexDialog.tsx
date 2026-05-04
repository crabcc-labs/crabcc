import { useEffect, useState } from "react";
import { RefreshCw, X } from "lucide-react";
import { api, type ReindexReport } from "../api";
import { Icon } from "./icons";
import { logMount, logUnmount, logUserAction } from "../lifecycle";
import { Button } from "./ui/button";
import { cn } from "../lib/cn";

// Tailwind class strings hoisted to module scope so the per-render
// shape stays `&'static str`-equivalent across runs. Modal layout
// matches the prior `.modal-*` rules pixel-for-pixel — see
// styles.css change for the corresponding deletes (track B.4).
const OVERLAY = cn(
  "fixed inset-0 z-[100]",
  "bg-black/50",
  "flex items-center justify-center",
);

const BODY = cn(
  "bg-card text-foreground border border-border rounded-lg",
  "w-[80vw] max-w-[780px] max-h-[80vh]",
  "flex flex-col",
);

const HEADER = cn(
  "flex justify-between items-center",
  "px-[18px] py-3.5 border-b border-border",
);

const CONTENT = "px-[18px] py-3.5 overflow-auto flex-1";

const FOOTER = cn(
  "px-[18px] py-2.5 border-t border-border",
  "flex justify-end gap-2",
);

// Pre-formatted blocks — same look as the prior `.reindex-stats` /
// `.reindex-logs` rules. `whitespace-pre-wrap` only on the logs
// pre below since the stats pre uses tab/space layout that should
// not wrap.
const PRE_BASE = cn(
  "font-mono text-xs leading-relaxed",
  "bg-background border border-border rounded",
  "p-2 my-2",
);

export function ReindexDialog({ onClose }: { onClose: () => void }) {
  const [running, setRunning] = useState(false);
  const [report, setReport] = useState<ReindexReport | null>(null);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    logMount("ReindexDialog");
    return () => logUnmount("ReindexDialog");
  }, []);

  const run = async () => {
    logUserAction("reindex started");
    setRunning(true);
    setErr(null);
    setReport(null);
    try {
      const r = await api.reindex();
      setReport(r);
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setRunning(false);
    }
  };

  return (
    <div className={OVERLAY} onClick={onClose}>
      <div className={BODY} onClick={(e) => e.stopPropagation()}>
        <header className={HEADER}>
          <strong>
            <Icon of={RefreshCw} size={14} aria-hidden="true" /> Re-index PWD
          </strong>
          <Button
            variant="outline"
            size="icon"
            onClick={onClose}
            aria-label="Close"
          >
            <Icon of={X} size={14} />
          </Button>
        </header>
        <div className={CONTENT}>
          <p>
            {running
              ? "Running `crabcc index` — this may take a few seconds on a large repo."
              : report
                ? `Done in ${report.elapsed_ms} ms.`
                : err
                  ? `Re-index failed: ${err}`
                  : "Click Run to start a full re-index against the server's PWD."}
          </p>
          {report && (
            <pre className={PRE_BASE}>
              <b>root</b>: {report.root}
              {"\n"}
              <b>files_indexed</b>: {String(report.stats.files_indexed ?? "?")}
              {"  "}
              <b>symbols</b>: {String(report.stats.symbols ?? "?")}
              {"  "}
              <b>edges</b>: {String(report.stats.edges ?? "?")}
            </pre>
          )}
          <h4
            className={cn(
              "text-[11px] uppercase text-muted",
              "tracking-wider mt-3.5 mb-1.5",
            )}
          >
            Tail of log output
          </h4>
          <pre
            className={cn(PRE_BASE, "max-h-[300px] overflow-auto whitespace-pre-wrap")}
          >
            {report?.logs.join("\n") ?? "— logs land here after the run completes —"}
          </pre>
        </div>
        <footer className={FOOTER}>
          <Button onClick={run} disabled={running}>
            {running ? "Running…" : report ? "Run again" : "Run"}
          </Button>
        </footer>
      </div>
    </div>
  );
}
