# Manual test checklist — Ollama auth stack (issue #105)

End-to-end smoke for the bundled Compose stack + crabcc CLI surface.
Run on a fresh shell with no preconditions other than Docker / OrbStack
available and `crabcc` on `PATH` (or pass `~/.cargo/bin/crabcc` explicitly).

Tick boxes as you go. Time budget: ~10–15 min on first run (model pull
dominates), ~3 min on subsequent runs (model cache warm).

---

## 0. Preflight

- [ ] `docker --version` returns Docker 24+ (or OrbStack equivalent).
- [ ] `docker compose version` returns Compose v2.20+ (`env_file` `path:` syntax requires this).
- [ ] On macOS: confirm OrbStack is the runtime if installed —
  ```bash
  ls ~/.orbstack/run/docker.sock && echo "OrbStack active"
  ```
- [ ] `crabcc --version` resolves (≥ 2.7.0).
- [ ] Working directory: this repo's root.

---

## 1. Shared network

- [ ] Create the cross-stack bridge:
  ```bash
  install/init-shared-network.sh
  ```
  Expect `created network crabcc-shared` (first run) or `network crabcc-shared already exists` (re-run).
- [ ] Re-run with `--info`; expect `name=crabcc-shared driver=bridge scope=local containers=0`.

---

## 2. Key bootstrap (Phase 1)

- [ ] Generate keys:
  ```bash
  install/ollama-stack/init-keys.sh
  ```
  Expect a printed `LITELLM_MASTER_KEY: sk-…` block and chmod-400 save instructions for `~/.crabcc.local.api-key`.
- [ ] Confirm `install/ollama-stack/.env` exists, mode 600, contains both `OLLAMA_API_KEY=` and `LITELLM_MASTER_KEY=sk-` lines.
- [ ] Confirm `git status` does **not** list `install/ollama-stack/.env` (gitignored).
- [ ] Re-run with `--rotate`; expect the master key in `.env` to change.
- [ ] Run `--quiet`; expect a single line of stdout (just the master key).

---

## 3. Stack up via Compose

- [ ] Boot the stack:
  ```bash
  cd install/ollama-stack
  docker compose up -d --wait
  ```
  Expect three healthy services: `ollama`, `caddy`, `litellm` (60–90 s on first run; pulls ~3 GB of images).
- [ ] `docker compose ps` shows three rows, all `healthy`.
- [ ] `docker compose ps --format json` is parseable JSON (one line per container).
- [ ] Container labels visible — issue #105 verification:
  ```bash
  docker inspect $(docker compose ps -q) --format '{{.Name}} {{.Config.Labels}}' | grep com.crabcc
  ```
  Each container should show `com.crabcc.project=crabcc`, `com.crabcc.stack=ollama-auth`, `com.crabcc.role=…`, `com.crabcc.issue=#105`.

---

## 4. Auth gate behaves

Caddy should reject unauth requests on `/api` and `/v1`, accept them with the right Bearer token.

- [ ] `curl -i http://localhost:11435/api/tags` → `401 Unauthorized`.
- [ ] `curl -i http://localhost:11435/v1/models` → `401 Unauthorized`.
- [ ] With auth (replace with actual key from `.env`):
  ```bash
  source install/ollama-stack/.env
  curl -s -H "Authorization: Bearer $OLLAMA_API_KEY" http://localhost:11435/api/tags | jq .
  ```
  Expect `{ "models": [...] }` (empty array on first run before model pull).
- [ ] Caddy `/healthz` (internal-only) is unauthenticated:
  ```bash
  docker compose exec caddy wget -qO- http://localhost:11434/healthz
  ```
  Expect `ok`.

---

## 5. LiteLLM OpenAI-compatible front

- [ ] Pull a model into the running container:
  ```bash
  docker compose exec ollama ollama pull qwen2.5-coder
  ```
- [ ] `curl` LiteLLM `/v1/models` with master key:
  ```bash
  source install/ollama-stack/.env
  curl -s http://localhost:4000/v1/models -H "Authorization: Bearer $LITELLM_MASTER_KEY" | jq .
  ```
  Expect three model entries: `ollama/llama3.2`, `ollama/qwen2.5-coder`, `ollama/nomic-embed-text` (the pulled one is `ready`).
- [ ] Send a chat completion:
  ```bash
  curl -s http://localhost:4000/v1/chat/completions \
    -H "Authorization: Bearer $LITELLM_MASTER_KEY" \
    -H 'Content-Type: application/json' \
    -d '{"model":"ollama/qwen2.5-coder","messages":[{"role":"user","content":"reply with the single word PONG"}]}' | jq -r '.choices[0].message.content'
  ```
  Expect a body containing `PONG` (model may add quotes or punctuation).

---

## 6. crabcc CLI integration (Phase 3)

- [ ] `crabcc ollama-stack status` returns a JSON array, one entry per running container, with `name`, `image`, `status`, `health`, `ports`, `networks` populated.
- [ ] `crabcc ollama-stack logs caddy --tail 20` prints recent Caddy log lines (passthrough, not JSON).
- [ ] `crabcc ollama-stack pull` runs `docker compose pull` and prints `{"ok":true}`.
- [ ] `crabcc ollama-stack down` then `crabcc ollama-stack up` cycles the stack; `up` returns `{"duration_ms":N,"services_healthy":["caddy","litellm","ollama"]}`.

---

## 7. ccc combo CLI (Phase 3)

- [ ] `ccc setup --ollama-status` prints the same JSON as `crabcc ollama-stack status`.
- [ ] `ccc setup --ollama-pull`, `--ollama-up`, `--ollama-down`, `--ollama-down-volumes` route to the right `crabcc ollama-stack` op.
- [ ] `ccc setup --help` shows all five `--ollama-*` flags under the existing `setup_what` mutex group.

---

## 8. Agent --backend ollama (Phase 4)

- [ ] Dry-run, no docker required:
  ```bash
  crabcc agent --run "list functions in lib.rs" --backend ollama --dry-run
  ```
  Expect the planned-invocation banner with `model: ollama/qwen2.5-coder` (defaulted from `DEFAULT_OLLAMA_MODEL`), no compose calls.
- [ ] Real run with stack already up:
  ```bash
  crabcc agent --run "ping" --backend ollama
  ```
  Expect a one-liner like `crabcc agent: ollama stack ready (3 services, <N> ms)` printed to stderr before the agent banner. Run output streams as usual.
- [ ] Confirm the `meta.json` records the backend:
  ```bash
  cat ~/.crabcc/agents/<run-id>/meta.json | jq '.backend, .model'
  ```
  Expect `"ollama"` and `"ollama/qwen2.5-coder"`.

---

## 9. Failure modes (negative coverage)

- [ ] With Docker NOT running: `crabcc agent --run "ping" --backend ollama` exits non-zero with the OS-aware install hint (OrbStack on macOS, Docker Engine link on Linux).
- [ ] With the stack DOWN: `crabcc agent --run "ping" --backend ollama` triggers `ollama_stack::ensure_up()`, brings the stack up, and proceeds. Re-run is idempotent.
- [ ] Tampered key — change `OLLAMA_API_KEY` in `.env` then re-up; the LiteLLM-side calls fail with 401 from the upstream Caddy. (Fix: rotate via `init-keys.sh --rotate` then `crabcc ollama-stack up`.)

---

## 10. Tear-down

- [ ] `crabcc ollama-stack down` (or `ccc setup --ollama-down`); `docker compose ps` shows zero rows.
- [ ] Volumes preserved by default; `docker compose ps -q` is empty but `docker volume ls | grep crabcc-ollama-stack` still lists `ollama_models`, `caddy_data`, `caddy_config`.
- [ ] Hard reset: `crabcc ollama-stack down --volumes` (or `ccc setup --ollama-down-volumes`); volumes wiped.
- [ ] `install/init-shared-network.sh --rm` removes the bridge (only after the stack is down — would refuse if containers were attached).

---

## 11. Observability spot-checks

The driver emits six tracing event discriminators under `target=crabcc_core::ollama_stack`. Run with `RUST_LOG=crabcc_core::ollama_stack=info` to see them:

- [ ] `RUST_LOG=crabcc_core::ollama_stack=info crabcc ollama-stack up 2>&1 | grep ollama_stack` — expect at least: `ollama_stack.detect`, `ollama_stack.up.start`, `ollama_stack.up.done`, then one `ollama_stack.container_info` per service.
- [ ] All events carry a `request_id` field (auto-generated `ols-<nanos:hex>` when not supplied).

---

## 12. Image labels (issue #105 / docker hygiene)

Only relevant if you've run `task images-build` (the bundled Compose pulls upstream images that don't carry our labels).

- [ ] `task images-build` succeeds, produces `crabcc:local`, `crabcc:<version>`, `crabcc:<git-sha>`.
- [ ] `task images-inspect` shows OCI labels (`org.opencontainers.image.{title,version,revision,…}`) plus crabcc-specific (`com.crabcc.role=binary`, `com.crabcc.issue=#105`, `com.crabcc.build.local=true`) and a reasonable `size_mb` (~80–120 MB for the Debian-slim runtime).
- [ ] `task images-build-nocache` rebuilds with `--no-cache --pull` on every layer; image sha changes, labels persist.

---

## Exit criteria

All 12 sections green = stack is good for the issue #105 PR review. Any red box → log details and revert your changes; do not merge.

---

## Appendix B — LiteLLM API-key + model passthrough audit (PR #127 follow-up)

Specifically verifies that `crabcc agent --backend ollama` reaches
the LiteLLM proxy with the correct `Authorization` header and routes
to the correct backing model. Independent of the broader
end-to-end checklist above; runs in ~2 minutes once the stack is up.

The wire path under audit:

```
crabcc agent --backend ollama
  → agent.rs SubprocessRuntime::run forwards OLLAMA_BASE_URL +
    OLLAMA_API_KEY to the spawned `claude` (or future `cua`) child
  → child issues HTTPS to ${OLLAMA_BASE_URL}/v1/chat/completions
    with `Authorization: Bearer ${OLLAMA_API_KEY}` (LiteLLM convention)
  → Caddy on :443 → LiteLLM on :4000 (model alias resolves)
  → ollama on :11434 (actual generation)
```

If any of those four hops drops auth or rewrites the model wrong,
the test fails.

### B.1 — Preflight (the stack must already be up)

- [ ] `crabcc ollama-stack status --json` reports `state="running"` for
  caddy / litellm / ollama. If not, `crabcc ollama-stack up` first.
- [ ] `cat ~/.crabcc.local.api-key` exists and is mode `0400`
  (chmod check; the file is the source of truth for OLLAMA_API_KEY).
- [ ] `echo $OLLAMA_BASE_URL` and `echo $OLLAMA_API_KEY` resolve in
  the test shell. If not: `eval "$(install/ollama-stack/init-keys.sh
  --print-env)"` (or whichever helper your local checkout exposes).

### B.2 — LiteLLM auth + alias (raw curl, no agent)

This isolates the proxy from the agent runtime. Confirms the same
env vars the runtime forwards work end-to-end against LiteLLM.

```bash
# 1. With the right key — should return JSON with `choices[0].message.content`.
curl -sS \
  -H "Authorization: Bearer $OLLAMA_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"qwen2.5-coder","messages":[{"role":"user","content":"reply with the word PONG only"}]}' \
  "$OLLAMA_BASE_URL/v1/chat/completions" \
  | jq '.choices[0].message.content'
# expected: "PONG" (or PONG-with-trailing-text on some quants)

# 2. With a wrong key — must return 401 (proves auth is enforced).
curl -sS -o /dev/null -w '%{http_code}\n' \
  -H "Authorization: Bearer NOT-A-REAL-KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"qwen2.5-coder","messages":[{"role":"user","content":"x"}]}' \
  "$OLLAMA_BASE_URL/v1/chat/completions"
# expected: 401

# 3. Wrong model name — must return 4xx, NOT silently fall through.
curl -sS -o /dev/null -w '%{http_code}\n' \
  -H "Authorization: Bearer $OLLAMA_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"this-model-does-not-exist","messages":[{"role":"user","content":"x"}]}' \
  "$OLLAMA_BASE_URL/v1/chat/completions"
# expected: 400 or 404 from LiteLLM, NOT 500 / hang.
```

- [ ] (1) returns the expected token (`PONG` substring acceptable)
- [ ] (2) returns HTTP 401
- [ ] (3) returns HTTP 4xx with a clear LiteLLM error body

### B.3 — Agent-runtime forwarding (dry-run)

The dry-run path doesn't spawn the LLM but DOES surface which env
vars + model the runtime resolved. Catches regressions in
`agent.rs::SubprocessRuntime::run`'s `auth_vars.extend(...)` block.

```bash
crabcc agent --run "noop" --dry-run
# (default backend is now ollama since v2.8.x)
```

Expected stdout banner:

- [ ] `runtime: subprocess (host)` line present
- [ ] `model: ollama/qwen2.5-coder (default)` line present
- [ ] `crabcc agent: model: ollama:qwen2.5-coder (params: 7B (Q4_K_M
      default), context: 32k, docs: …)` (the per-model banner from
      `model_info::banner_line`, gated on the seed file existing —
      run `crabcc model-info seed-default` once if missing).

### B.4 — Live agent run hits the proxy with the right key

Strace-style check via the proxy's request log instead of the agent
side. Tail Caddy + LiteLLM logs while the agent runs once, confirm
exactly one request landed with the right shape.

Terminal A:
```bash
crabcc ollama-stack logs --service litellm --follow | tee /tmp/litellm.log
```

Terminal B (in any indexed crabcc-touched repo):
```bash
crabcc agent --run "reply with the word PONG only" \
            --no-refresh \
            --no-repomix    # skip the per-agent bundle for a fast loop
```

After the agent exits, in terminal A:

- [ ] Exactly one `POST /v1/chat/completions` line, status 200
- [ ] `model="qwen2.5-coder"` (post-LiteLLM-alias resolution — NOT
      `ollama/qwen2.5-coder` which is the alias name)
- [ ] No `401 Unauthorized` in the window
- [ ] `Authorization` header parsed (LiteLLM logs the user id derived
      from the bearer; should be `crabcc-local` or whichever id the
      bundled init-keys.sh assigned — NOT `anonymous`)

### B.5 — Override flow (sanity)

Confirm `--model` actually flips the request payload model.

```bash
crabcc agent --run "say PING" --backend ollama --model ollama/llama3.2 --no-refresh --no-repomix
```

Watching terminal A:

- [ ] Request payload's `model` field is `llama3.2` (post-alias)
- [ ] If the model isn't pulled locally: LiteLLM returns 4xx,
      `crabcc agent-guard` doesn't classify the run as zombie
      (it exited cleanly with exit code 1).

### B.6 — Negative: run with no env vars

Proves the auth-passthrough block is the only thing between the
runtime and LiteLLM — i.e. nothing is hardcoded.

```bash
unset OLLAMA_BASE_URL OLLAMA_API_KEY
crabcc agent --run "noop" --no-refresh --no-repomix 2>&1 | tail -5
```

- [ ] Agent exits non-zero
- [ ] stderr contains an error mentioning either the missing URL or
      the missing key — NOT a generic "claude binary not found" or a
      silent fallback to Anthropic.

### Audit verdict

Mark the issue audit complete when:

- [ ] B.1 + B.2 all green (the proxy is wired correctly stand-alone)
- [ ] B.3 + B.4 green (the runtime forwards the same vars cleanly)
- [ ] B.5 green (override flow works)
- [ ] B.6 green (no hidden hardcoded fallback)

If any item is red, the issue #105 audit is **not** complete —
attach the failing terminal capture to the issue and reopen the
relevant code path before closing.
