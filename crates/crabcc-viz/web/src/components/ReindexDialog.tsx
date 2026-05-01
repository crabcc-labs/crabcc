import { useEffect, useState } from "react";
import { RefreshCw, X } from "lucide-react";
import { api, type ReindexReport } from "../api";
import { Icon } from "./icons";
import { logMount, logUnmount, logUserAction } from "../lifecycle";

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
    <div className="modal" onClick={onClose}>
      <div className="modal-body" onClick={(e) => e.stopPropagation()}>
        <header>
          <strong>
            <Icon of={RefreshCw} size={14} aria-hidden="true" /> Re-index PWD
          </strong>
          <button onClick={onClose} aria-label="Close">
            <Icon of={X} size={14} />
          </button>
        </header>
        <div className="modal-content">
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
            <pre className="reindex-stats">
              <b>root</b>: {report.root}
              {"\n"}
              <b>files_indexed</b>: {String(report.stats.files_indexed ?? "?")}
              {"  "}
              <b>symbols</b>: {String(report.stats.symbols ?? "?")}
              {"  "}
              <b>edges</b>: {String(report.stats.edges ?? "?")}
            </pre>
          )}
          <h4>Tail of log output</h4>
          <pre className="reindex-logs">
            {report?.logs.join("\n") ?? "— logs land here after the run completes —"}
          </pre>
        </div>
        <footer>
          <button className="run" onClick={run} disabled={running}>
            {running ? "Running…" : report ? "Run again" : "Run"}
          </button>
        </footer>
      </div>
    </div>
  );
}
