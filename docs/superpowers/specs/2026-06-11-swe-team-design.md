# SWE-Team (rig) — Design

- **Date:** 2026-06-11
- **Status:** approved design (brainstormed collaboratively), pre-implementation
- **Crate:** `crates/swe-team` (rig-core 0.38.2; standalone, workspace-excluded; shells out to the `crabcc` binary)
- **v0 already built:** commit `6104158f` — 3 coders + synthesizer pipeline, compiles, reaches the gateway. This spec is the full system v0 grows into.

## Context

An agent-heavy solo/small-team wants a multi-agent SWE team that embodies the superpowers workflow (brainstorm → plan-approve → subagent-driven implementation → self-review → commit → ping lead), runs on cheap open models for the bulk work, and is fully observable. Built on **rig** (Rust) for the executor + a **translation layer** that mirrors the run into AgentField's panel and the eye/viz dashboards. Reference: github.com/obra/superpowers.

## Architecture

Substrate: **rig** (Rust), orchestrated as a LangGraph-style state graph in code. One `openai::Client` pointed at the gateway (`Client::builder().api_key().base_url().build()?.completions_api()`); each node picks its own model + sampling params.

**The graph (superpowers generic flow):**

```
task + repo
 1. PLAN        Planner agent drafts an implementation plan (read-only repo tools).
 2. LEAD GATE   Lead Dev reviews the plan: APPROVE | REVISE(notes) | STOP("rule violation").
                - "timetravel": on a project-rule violation it can hard-stop or rewind to PLAN.
                - On APPROVE it ALSO pre-configures the team (see Lead Dev below) and pre-injects context.
 3. FANOUT      3 Coders IN PARALLEL ("maximum fanout"), each = approved plan + pre-injected ctx + read tools,
                lenses: safety/correctness · performance · simplicity/readability  -> 3 candidate unified diffs.
 4. SYNTH       Synthesizer reconciles plan + 3 diffs -> ONE best-of-three unified diff.
 5. REVIEW      deepseek-v4-flash reviewer: chill/not-strict EXCEPT paranoid on security + UI.
                APPROVE | REQUEST-CHANGES(notes) -> loop back to SYNTH (capped).
 6. SELF-REVIEW Synthesizer self-reviews the final diff against the plan + review notes.
 7. COMMIT      Emit the diff (NOT auto-applied) + a commit message; optionally `git apply` behind a flag.
 8. PING LEAD   Final summary back to the Lead Dev node (and the panel).
```

All gates loop with a round cap; on exhaustion, proceed with a printed warning (never hang).

**Lead Dev (expanded role):** before the coders start it (a) **pre-configures the team's model params** — `reasoning` (effort/max-reasoning-tokens/enabled), `max_tokens`, `temperature`, `top_p`, `seed`, `tool_choice`, `tools` (OpenAI request shape) — per node; and (b) **pre-injects context** by running crabcc `sym`/`refs`/`outline` queries for the symbols the plan names, so coders start with the relevant code already in hand rather than each re-fetching.

**Models (via gateway / OpenRouter, env-configurable):**
- Coders: cheap open model (`SWE_CODER_MODEL`, default a local qwen via LiteLLM).
- Lead Dev + Synthesizer: stronger (`SWE_LEAD_MODEL`/`SWE_SYNTH_MODEL`, e.g. sonnet/opus via gateway).
- Reviewer: `deepseek-v4-flash` via OpenRouter (`SWE_REVIEW_MODEL`) — needs the model added to `install/ollama-stack/litellm.config.yaml` + `OPENROUTER_API_KEY`.

**Read tools (shared, read-only; dogfood crabcc):** `crabcc_sym`, `crabcc_refs`, `crabcc_outline`, `read_file` (already in v0's `src/tools.rs`).

**Observability — translation layer (the "connect into AgentField" piece):**
The graph emits a structured **trace event** per node transition (node id, role, model+params, inputs digest, output digest, gate decision, timing). A translation layer fans those events into:
- **eye + viz** — the existing crabcc dashboards (render the graph + per-node status live).
- **AgentField panel** — translate rig events into AgentField's trace/ingestion format so the same run shows in the panel (DID/VC provenance if available).

This keeps rig as the executor while making the run visible everywhere. The event schema is rig-owned; the AgentField + eye/viz emitters are adapters over it.

## Components / files (extend the v0 crate)

- `src/graph.rs` — the state graph + node transitions + round caps (the superpowers flow).
- `src/agents.rs` — preambles + per-node param config for planner, lead-dev, 3 coders, synthesizer, reviewer.
- `src/lead.rs` — Lead Dev: plan review/gate + team pre-config + crabcc ctx pre-injection.
- `src/tools.rs` — crabcc-backed read tools (exists).
- `src/trace.rs` — the trace event type + emitters: stdout, eye/viz, AgentField adapter.
- `src/main.rs` — CLI (`--repo`, task, `--apply`, model/observability env), wires the graph.

## Open implementation-research (BEFORE the translation layer is planned)

1. **AgentField ingestion API** — how a non-AgentField process emits a trace/run into the panel (HTTP endpoint? SDK? DID/VC chain shape?). Verify against `~/workspace/agentfield-sdk` before designing `trace.rs`'s AgentField emitter.
2. **eye + viz event format** — how `crabcc.app-eye` / `crabcc-viz` ingest live agent/graph events (WS? a JSONL feed? the existing `/eye` token-gated stream?). Verify before the eye/viz emitter.
3. **deepseek-v4-flash via the gateway** — confirm OpenRouter routing + add the model alias to litellm.config; confirm rig's per-request `reasoning`/sampling params pass through `.completions_api()`.
4. **rig per-node sampling/reasoning params** — confirm how to set temperature/top_p/seed/reasoning/tool_choice per agent in rig 0.38 (AgentBuilder vs PromptRequest), since the Lead-Dev pre-config depends on it.

## Verification

- `cargo build`/`clippy` clean on the crate (workspace-excluded).
- A dry run with the gateway up: `swe-team --repo <path> "<task>"` produces a plan, a lead-dev decision, 3 candidate diffs, a synthesized diff, a deepseek review, and emits trace events; `--apply` optionally `git apply`s.
- Trace events appear in eye/viz and (once the adapter lands) the AgentField panel.

## Build path (superpowers, recursively)

brainstorm (done) → this spec → resolve the 4 research items → writing-plans (bite-sized TDD) → **subagent-driven-development with maximum fanout** (the very flow this team embodies) → self-review → commit. The translation-layer pieces are gated behind the API research so we don't fabricate AgentField/eye/viz integration.
