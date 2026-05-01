// One model row — provider, name, optional params/context, plus a
// reachability dot.
//
// The wire shape doesn't yet carry probe data (latency / last-probe);
// we colour the dot on a presence-only heuristic: ollama is "local"
// (assume reachable), other providers are "cloud" (unknown). This
// lights up the affordance without over-claiming. A future API
// addition can promote the dot to "live", "down", "stale" with real
// probe results.

import { memo } from "react";
import type { AgentModelEntry } from "./types";

interface Props {
  model: AgentModelEntry;
  selected: boolean;
  expanded: boolean;
  onPick(): void;
}

export const ModelRow = memo(function ModelRow({
  model,
  selected,
  expanded,
  onPick,
}: Props) {
  const cls =
    "agents-row model" +
    (selected ? " selected" : "") +
    (expanded ? " expanded" : "");
  const reachClass = reachabilityClass(model.provider);
  return (
    <button type="button" className={cls} onClick={onPick}>
      <span
        className={`agents-model-dot ${reachClass}`}
        title={reachabilityTitle(model.provider)}
      />
      <span className="agents-model-provider">{model.provider}</span>
      <span className="agents-model-name">{model.name}</span>
      {model.params ? (
        <span className="agents-tag">{model.params}</span>
      ) : null}
      {model.context !== null ? (
        <span className="agents-tag">{Math.round(model.context / 1024)}k ctx</span>
      ) : null}
    </button>
  );
});

function reachabilityClass(provider: string): string {
  const p = provider.toLowerCase();
  if (p === "ollama") return "local";
  if (p === "anthropic" || p === "openai" || p === "groq") return "cloud";
  return "unknown";
}

function reachabilityTitle(provider: string): string {
  const p = provider.toLowerCase();
  if (p === "ollama") return "local backend (Ollama)";
  if (p === "anthropic" || p === "openai" || p === "groq") return `cloud backend (${provider})`;
  return `${provider} backend`;
}
