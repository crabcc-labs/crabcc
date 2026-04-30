import { memo, useEffect, useState } from "react";
import { api } from "../api";

// Ollama auth-stack key reveal/copy. Reads from /api/ollama-key
// which tails ~/.crabcc.local.api-key (chmod 0400, auto-persisted by
// init-keys.sh). Local-loopback dashboard only — exposing the key
// here is no worse than `cat ~/.crabcc.local.api-key`. The default
// state is masked; user clicks an eye toggle to reveal, copy button
// pushes to clipboard via navigator.clipboard.
export const OllamaKeyPanel = memo(function OllamaKeyPanel() {
  const [snap, setSnap] = useState<{
    present: boolean;
    path: string;
    mode: string | null;
    mtime_secs: number | null;
    size_bytes: number | null;
    key: string | null;
  } | null>(null);
  const [revealed, setRevealed] = useState(false);
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    const load = () => {
      api
        .ollamaKey()
        .then((r) => {
          if (!alive) return;
          setSnap(r);
          setError(null);
        })
        .catch((e) => alive && setError(String(e)));
    };
    load();
    const t = window.setInterval(load, 30_000);
    return () => {
      alive = false;
      window.clearInterval(t);
    };
  }, []);

  if (error) {
    return <div className="empty">ollama-key unavailable: {error}</div>;
  }
  if (!snap) {
    return <div className="empty">loading…</div>;
  }
  if (!snap.present) {
    return (
      <div className="empty">
        no key at <code>{snap.path}</code> — run{" "}
        <code>install/ollama-stack/init-keys.sh</code>
      </div>
    );
  }

  const masked = snap.key
    ? `${snap.key.slice(0, 6)}${"•".repeat(Math.max(0, snap.key.length - 10))}${snap.key.slice(-4)}`
    : "";
  const display = revealed ? snap.key ?? "" : masked;
  const modeOk = (snap.mode ?? "0000").endsWith("400");

  const onCopy = async () => {
    if (!snap.key) return;
    try {
      await navigator.clipboard.writeText(snap.key);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      setError("clipboard write blocked — copy manually");
    }
  };

  const ageHours = snap.mtime_secs
    ? Math.round((Date.now() / 1000 - snap.mtime_secs) / 3600)
    : null;

  return (
    <div className="ollama-key">
      <div className="profiles-dir">
        <code>{snap.path}</code>
      </div>
      <div className="key-row">
        <code className="key-display">{display}</code>
        <button
          className="key-btn"
          onClick={() => setRevealed((v) => !v)}
          title={revealed ? "Hide" : "Reveal"}
        >
          {revealed ? "hide" : "reveal"}
        </button>
        <button className="key-btn" onClick={onCopy} title="Copy to clipboard">
          {copied ? "copied!" : "copy"}
        </button>
      </div>
      <div className="key-meta">
        <span className={`key-mode ${modeOk ? "ok" : "warn"}`}>
          mode: {snap.mode ?? "?"}
          {!modeOk && " (expected 0400)"}
        </span>
        {ageHours != null && <span>generated {ageHours}h ago</span>}
        {snap.size_bytes != null && <span>{snap.size_bytes} bytes</span>}
      </div>
    </div>
  );
});
