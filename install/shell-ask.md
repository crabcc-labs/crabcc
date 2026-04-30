# shell-ask integration

[`shell-ask`](https://github.com/egoist/shell-ask) is a CLI that takes a
prompt + optional stdin context and asks an LLM. It supports OpenAI,
Claude, Ollama, Groq, and friends out of the box. crabcc ships a
shell-ask config (`install/shell-ask.json`) with **six reusable commands**
tuned for piping crabcc / ccc / telemetry output:

| Command                         | Pipes from                                | Output                                       |
|---------------------------------|-------------------------------------------|----------------------------------------------|
| `ask crabcc-explain`            | `crabcc sym X` / `ccc find X`             | 2–3 sentence plain-prose explainer + next step |
| `ask crabcc-cycles`             | `crabcc graph cycles`                     | Markdown table tagging intentional vs accidental |
| `ask crabcc-orphans`            | `crabcc graph orphans`                    | grouped: API / test helpers / dead code      |
| `ask telemetry-digest`          | `tail -n200 .crabcc/telemetry.jsonl`      | per-KPI count + p50/p95 latency table         |
| `ask cm`                        | `git diff` / `git diff --cached`          | Conventional-Commits subject + bullets        |
| `ask openapi-drift`             | `git diff crates/crabcc-mcp/openapi.yaml` | breaking / compatible / review classification |

## Install

```bash
# 1. Install shell-ask (npm):
npm i -g shell-ask

# 2. Drop the crabcc commands into your shell-ask config:
mkdir -p ~/.config/shell-ask
cp install/shell-ask.json ~/.config/shell-ask/config.json
# (or merge if you already have a config — the `commands` array can extend an existing one)

# 3. Pull the recommended Ollama model (skip if you already have it):
ollama pull qwen3.5:35b-a3b-coding-nvfp4

# 4. Verify:
crabcc sym Store | ask crabcc-explain
```

## Examples

```bash
# Explain a symbol in human terms.
ccc find Codec | ask crabcc-explain

# Triage a cycles report.
crabcc graph cycles | ask crabcc-cycles

# Conventional-Commits draft from staged diff.
git diff --cached | ask cm

# Latency digest from the dashboard's telemetry sink.
tail -n 500 .crabcc/telemetry.jsonl | ask telemetry-digest

# One-off question with web search.
ask -s "what does the rust `Pin` type guarantee?"
```

## Default model

The shipped config sets `default_model` to
`ollama-qwen3.5:35b-a3b-coding-nvfp4` — the NVFP4-quantized Qwen3.5
variant that Ollama 0.19+ recommends with its MLX backend on Apple
Silicon (see [`install/external-tooling.md`](./external-tooling.md)
for the rationale).

Override per-call via `-m`:

```bash
git diff | ask -m claude-sonnet-4-6 cm
ccc find Foo | ask -m gpt-4o crabcc-explain
```

## Cross-references

- [`install/external-tooling.md`](./external-tooling.md) — Ollama MLX
  + Jon Brown's macOS guide + this doc, in one survey.
- [`Taskfile.yml`](../Taskfile.yml) — `task ask` / `task ask-config`
  / `task ask-install` wrappers.
- [Issue #90](https://github.com/peterlodri-sec/crabcc/issues/90) —
  `.crabcc/telemetry.jsonl` (the source `ask telemetry-digest` reads).
