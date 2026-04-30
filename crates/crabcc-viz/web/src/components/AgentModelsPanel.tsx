import { memo, useEffect, useState } from "react";
import { api, type AgentModelEntry } from "../api";

// Lists per-model metadata files at $CRABCC_HOME/models/.model.<provider>.<name>.info.
// The bundled default is `ollama:qwen2.5-coder` (the new default backend
// since v2.8.x). Click a docs link to jump to the official model docs.
export const AgentModelsPanel = memo(function AgentModelsPanel() {
  const [models, setModels] = useState<AgentModelEntry[]>([]);
  const [dir, setDir] = useState<string>("");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    const load = () => {
      api
        .agentModels()
        .then((r) => {
          if (!alive) return;
          setModels(r.models);
          setDir(r.dir);
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
    return <div className="empty">models unavailable: {error}</div>;
  }
  if (models.length === 0) {
    return (
      <div className="empty">
        no models cataloged. Run <code>crabcc model-info seed-default</code>
      </div>
    );
  }
  return (
    <div className="scroll">
      <div className="profiles-dir">
        <code>{dir}</code>
      </div>
      {models.map((m) => (
        <div key={m.file} className="model-row">
          <div className="model-head">
            <span className="model-provider">{m.provider}</span>
            <span className="model-name">{m.name}</span>
            {m.params && <span className="model-params">{m.params}</span>}
            {m.context != null && (
              <span className="model-ctx">
                ctx: {Math.round(m.context / 1024)}k
              </span>
            )}
          </div>
          {m.docs_first && (
            <a
              className="model-docs"
              href={m.docs_first}
              target="_blank"
              rel="noreferrer"
            >
              {m.docs_first}
            </a>
          )}
        </div>
      ))}
    </div>
  );
});
