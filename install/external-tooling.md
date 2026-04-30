# External tooling — survey + integration notes

Survey of three external sources cited as inputs to crabcc's
local-AI / dev-tooling story. One section each; ends with the changes
that landed on `refactor/ollama-mlx-shell-ask`.

## 1. Ollama MLX (preview, 2026-03-30)

**Source**: <https://ollama.com/blog/mlx>

Ollama 0.19 ships **MLX as the default backend on Apple Silicon** —
no opt-in flag, no env var, just upgrade. Headline numbers (M-series,
Qwen3.5-35B-A3B):

| Metric  | Ollama 0.18 (llama.cpp + Q4_K_M) | Ollama 0.19 (MLX + NVFP4) | Δ        |
|---------|----------------------------------|---------------------------|----------|
| Prefill | 1154 t/s                         | 1810 t/s                  | +57 %    |
| Decode  | 58 t/s                           | 112 t/s                   | +93 %    |

Headlines:

- **Quantization**: NVFP4 (NVIDIA's int4) replaces Q4_K_M for the
  recommended models. NVFP4 matches production-inference parity for
  shops that scale on NVIDIA hardware.
- **Caching**: cross-conversation cache reuse + intelligent
  checkpoints in long prompts. Big win for Claude Code / OpenClaw
  shared-system-prompt workflows.
- **Recommended model**: `qwen3.5:35b-a3b-coding-nvfp4` — needs a Mac
  with **>32 GB unified memory**.
- **No CLI flag to enable MLX** — it's automatic on Apple Silicon
  when the binary is 0.19+.

New launch syntax (one-shot daemon configured for a coding agent):

```bash
ollama launch claude   --model qwen3.5:35b-a3b-coding-nvfp4
ollama launch openclaw --model qwen3.5:35b-a3b-coding-nvfp4
```

**Caveats** (per the launch post):

- Preview release — production use is fine, but reproducible
  benchmarks across versions can shift.
- Pre-M5 chips don't get the new GPU Neural Accelerator boost
  (M1/M2/M3/M4 still see the 1.5–1.8× MLX gain, just not the
  additional accelerator delta).
- Models pulled before 0.19 are still GGUF; NVFP4 is a separate pull.

## 2. Jon Brown — Setting up Ollama on macOS

**Source**: <https://jonbrown.org/blog/setting-up-and-using-ollama-on-macos/>

Practical macOS-side guide. Confirms what `task ollama-bootstrap`
already does (the curl-piped install script + `ollama pull` flow);
adds two patterns worth adopting:

- **DMG install** as an alternative to the shell installer:
  <https://ollama.com/download>. Both methods install the CLI; the
  DMG also ships a tray app + auto-start launch agent.
- **Custom-provider integration** with downstream coding agents:
  point them at `http://127.0.0.1:11434/v1` (OpenAI-compatible API
  shape) and pick the local Ollama model from the agent's model
  picker. Works with OpenCode, Codex, Claude Code's custom-provider
  flow.

Nothing controversial. `task ollama-bootstrap` already covers the
substance.

## 3. shell-ask

**Source**: <https://github.com/egoist/shell-ask>

CLI wrapper that takes a prompt + optional stdin context and asks an
LLM. Multi-provider (OpenAI / Claude / Ollama / Groq / others).
Reusable commands defined in `~/.config/shell-ask/config.json`.

Key adoption patterns relevant to crabcc:

- **Pipes**: `crabcc sym Foo | ask "explain"` — JSON in, prose out.
- **Multi-file context**: `ask --files "src/*.rs" "outline this"`.
- **Built-in commit-msg helper**: `git diff | ask cm` (a worked
  example in their docs; we override the prompt with our
  Conventional-Commits + crate-scope conventions).
- **Web search via -s** (jina.ai-backed) — useful for one-off "what
  does this Rust type guarantee?" questions that don't need a model
  with a tool surface.

See [`install/shell-ask.md`](./shell-ask.md) for our six reusable
commands tuned for crabcc / ccc / telemetry output and the install
recipe.

---

## What landed on this branch

| File                                | Change                                                                               |
|-------------------------------------|--------------------------------------------------------------------------------------|
| `scripts/ollama-system-check.sh`    | Adds **MLX eligibility probe** — green when Apple Silicon + Ollama ≥ 0.19; yellow with upgrade hint otherwise. Adds the `qwen3.5:35b-a3b-coding-nvfp4` row (21 GB disk / 32 GB RAM) to the per-model requirements table. New row in the rendered output. |
| `install/shell-ask.json`            | New — six crabcc-flavored reusable commands (`crabcc-explain`, `crabcc-cycles`, `crabcc-orphans`, `telemetry-digest`, `cm`, `openapi-drift`). Default model = `ollama-qwen3.5:35b-a3b-coding-nvfp4`. |
| `install/shell-ask.md`              | New — install + usage guide for the integration.                                     |
| `install/external-tooling.md`       | This file.                                                                           |
| `Taskfile.yml`                      | New `ask`, `ask-install`, `ask-config` tasks (gated on `command -v ask`).            |

## Skipped (intentional)

- **No DMG-installer auto-detect** in `task ollama-bootstrap`.
  Detecting whether the user already has `Ollama.app` requires
  poking at `~/Applications/Ollama.app` + `launchctl list | grep
  ollama`, and the brew-install path is already idempotent (brew is
  a no-op when the binary is on PATH from any source). Not worth
  the script complexity.
- **No automatic `ollama launch claude` / `ollama launch openclaw`
  wiring**. Those new commands ship the daemon configured for a
  specific coding-agent workflow; crabcc's fan-out scripts don't
  use them. Mention in `external-tooling.md` (above) is enough.
- **No shell-ask hard dependency**. The `task ask*` targets are
  gated on `command -v ask`; users who don't install shell-ask see
  a clear error pointing at `npm i -g shell-ask`.
