# Flow token matrix — "with vs without crabcc hooks"

> Slow-cadence results doc. The **deterministic lane** is network-free and
> reproducible on any machine; the **deep external lanes** (Morph squeeze +
> real-tokenizer prompt-token counts) cost real tokens/secrets and so are
> refreshed only occasionally — update the tables below when you re-run them.
>
> Harness: [`scripts/bench-flow-matrix.sh`](../scripts/bench-flow-matrix.sh)
> (`task bench-flow-matrix`). Methodology mirrors
> [`docs/PERF-648-agent-shell-and-deps.md`](./PERF-648-agent-shell-and-deps.md):
> a clean `git archive HEAD` tree (no `target/` noise), `tokens = bytes/4`,
> three agent profiles whose command mixes come from
> `crates/crabcc-mcp/benches/agent_profiles.rs`.

## Lane 1 — deterministic (network-free, model-independent)

Replays each profile's command mix twice — vanilla (raw `grep`/`cat`/`find`)
and through the full `crabcc shell rewrite` pipeline (engine rewrite → RTK,
plus the `cat`→`read` path) — and reports tokens per profile. These byte
reductions are **tokenizer-independent**, so this lane is the headline result.

Repo: `crabcc` @ `main` · crabcc: `target/debug/crabcc` · Morph stage: **off**

| profile      | vanilla   | flow      | reduction |
|--------------|-----------|-----------|-----------|
| claude_code  |   138,036 |    40,189 |   **−71%** |
| nullclaw     |   101,969 |     3,036 |   **−97%** |
| zeroclaw     |   103,179 |     4,692 |   **−95%** |

Reproduce (no keys, no network):

```bash
cargo build -p crabcc-cli --bin crabcc
env -u MORPH_API_KEY -u OPENROUTER_API_KEY -u MODELS CRABCC_NO_MORPH=1 \
  CRABCC=target/debug/crabcc bash scripts/bench-flow-matrix.sh
```

## Lane 2 — Morph squeeze (deterministic + `MORPH_API_KEY`)

Same deterministic mix with the flow's Morph stage engaged. Costs Morph tokens;
sends clean-tree repo content to `api.morphllm.com`. Measured on a local run
(`crabcc` @ `main`, `target/debug/crabcc`, Morph **on**):

| profile      | vanilla   | flow (Morph on) | reduction |
|--------------|-----------|-----------------|-----------|
| claude_code  |   138,285 |          27,577 |   **−80%** |
| nullclaw     |   102,161 |           3,036 |   **−97%** |
| zeroclaw     |   103,381 |           2,929 |   **−97%** |

Morph deepens the engine+RTK flow further where there's prose left to squeeze:
claude_code **−71% → −80%** and zeroclaw **−95% → −97%** vs the Morph-off lane
above. nullclaw is already at the floor (its flow output is almost entirely
symbol-upgrade JSON), so Morph adds nothing there.

> Refresh with:
> ```bash
> MORPH_API_KEY=… CRABCC=target/release/crabcc bash scripts/bench-flow-matrix.sh
> ```

## Lane 3 — real-tokenizer prompt tokens (`OPENROUTER_API_KEY` + `MODELS`) — _deferred_

The same vanilla-vs-flow `claude_code` context, measured as the API's actual
`usage.prompt_tokens` per model (real tokenizers, not bytes/4). This is the only
model-*dependent* number, and it is **not essential**: lanes 1–2 are
model-independent, so this lane only restates "−80% bytes ≈ −80% real tokens."

> _Deferred._ The OpenRouter lane builds the request by slurping the full
> `claude_code` context into a `jq --rawfile` payload; on a memory-constrained
> machine this can OOM before printing rows, so it's left for a run on a roomy
> box (and a large-context model — the ~138K-token vanilla side overflows
> small-window models, returning `err`). Refresh with:
> ```bash
> OPENROUTER_API_KEY=… MODELS="anthropic/claude-haiku-4-5" \
>   CRABCC=target/release/crabcc bash scripts/bench-flow-matrix.sh
> ```

| model | vanilla ptok | flow ptok | reduction |
|-------|--------------|-----------|-----------|
| _(deferred — see note)_ | — | — | — |

## Notes

- The deep lanes (2 & 3) only *refine/confirm* lane 1: lane 1's byte
  reductions are model-independent; Morph adds further squeeze and OpenRouter
  re-states the reductions in real prompt tokens. None of them change the
  headline −71 / −97 / −95%.
- The harness is calibrated for the `crabcc` tree (symbols like `Store`,
  `Backend`). Running it against another repo via `REPO=…` works but the
  symbol-upgrade rewrites only fire on symbols that exist there, so adapt the
  profile command mixes before reading cross-repo numbers as comparable.
