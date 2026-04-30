---
description: Initialize crabcc symbol index in the current repo and run a first full index.
---

Run these steps:

1. Check that `crabcc` is on PATH. If not, build and link:
   ```
   cd ~/workspace/bin/crabcc && cargo install --path crates/crabcc-cli
   ```
2. From the user's repo root, run `crabcc index`.
3. Report indexed file count, symbol count, and DB size from `.crabcc/index.db`.
4. Suggest the user add `.crabcc/` to `.gitignore` if not already present.

## Optional: Ollama auth stack (issue #105)

If the user wants to run agents through a **local** Ollama backend
(no Anthropic API quota burn, fully offline-capable), offer to bring
up the bundled Compose stack. **Skip this step unless the user opts
in** — Docker is required and the first pull is ~3 GB.

Ask: *"Do you want to set up the local Ollama auth stack? (y/N)"*

If yes:

1. Verify Docker is reachable: `docker --version` and
   `docker compose version`. On macOS, prefer OrbStack
   (`brew install orbstack`); on Linux point at
   https://docs.docker.com/engine/install/. Stop until resolved.
2. ```
   crabcc install-claude --with-ollama-stack
   ```
   Materializes `~/.crabcc/ollama-stack/`, runs
   `docker compose up -d --wait`, reports services healthy.
3. ```
   ~/.crabcc/ollama-stack/init-keys.sh
   ```
   Writes `~/.crabcc/ollama-stack/.env` (mode 600). Print the
   master key to the user; suggest persisting to
   `~/.crabcc.local.api-key` with chmod 400.
4. Pull the default model (`qwen2.5-coder` — code-tuned):
   ```
   docker compose -f ~/.crabcc/ollama-stack/docker-compose.yml exec ollama ollama pull qwen2.5-coder
   ```
5. Verify: `crabcc ollama-stack status` returns three healthy
   containers, each carrying `com.crabcc.role` labels.

The user can now run offline agents with
`crabcc agent --backend ollama --run "..."`. Full reference:
`install/ollama-stack/OLLAMA-AUTH.md`.
