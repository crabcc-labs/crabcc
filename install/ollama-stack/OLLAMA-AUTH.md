# crabcc Ollama auth — guide

Single canonical doc for the Ollama auth Compose stack (issue #105).
Pairs with [`README.md`](./README.md) (quick-start) and
[`MANUAL_TEST_CHECKLIST.md`](./MANUAL_TEST_CHECKLIST.md) (e2e verification).

## TL;DR

```bash
# One-liner from a fresh checkout
crabcc install-claude --with-ollama-stack
~/.crabcc/ollama-stack/init-keys.sh

# Then use Ollama-backed agents:
crabcc agent --backend ollama --run "your prompt"
```

## Why

[Ollama](https://ollama.com) has **no native API-key auth**. As soon as
its `:11434` listener is reachable beyond loopback (shared dev box, LAN,
hosted runner) the endpoint is **open compute** for anyone who finds
the URL. Upstream's recommended workaround is a reverse proxy; this
stack pre-bakes that recipe and stacks an OpenAI-compatible front
([LiteLLM](https://www.litellm.ai/)) on top so existing OpenAI client
code works unchanged.

Reference: Damien Berezenko, *Ollama with ApiKey & LiteLLM Proxy*
(Feb 2025) — https://medium.com/@qdrddr/ollama-with-apikey-litellm-proxy-c675c32ce7e8

## What's in the stack

```
client                    Caddy                Ollama
   │ Authorization:         │ proxy with auth     │
   │ Bearer $LITELLM_…      │ check               │
   ▼                        ▼                     ▼
┌─────────┐  Bearer    ┌─────────┐   no-auth   ┌──────┐
│ LiteLLM │ ─────────► │ Caddy   │ ──────────► │Ollama│
│ :4000   │            │ :11435  │             │      │
└─────────┘            └─────────┘             └──────┘
   ▲                        ▲                     ▲
   │ users + crabcc         │ direct curl tests   │ never reachable
   │ talk here              │ welcome             │ from host
```

Three Compose services, all bundled in
[`docker-compose.yml`](./docker-compose.yml):

| service | image | purpose |
|---|---|---|
| `ollama`  | `ollama/ollama:latest`              | model server, internal-only |
| `caddy`   | `caddy:2-alpine`                    | reverse proxy enforcing `Authorization: Bearer ${OLLAMA_API_KEY}` on `/api` and `/v1` |
| `litellm` | `ghcr.io/berriai/litellm:main-stable` | OpenAI-compatible front, master-key auth, talks to Ollama via Caddy |

Ollama is **never exposed** to the host. Caddy publishes `:11435` for
direct curl tests. LiteLLM publishes `:4000` — that's the URL clients
(including `crabcc agent --backend ollama`) talk to.

## Installation

Two paths.

### Path A — Embedded materialization (recommended)

```bash
crabcc install-claude --with-ollama-stack
```

`crabcc install-claude` writes the Compose recipe (7 files) embedded in
the binary out to `~/.crabcc/ollama-stack/`, runs `docker compose up -d
--wait`, and reports services healthy. Idempotent — re-running picks up
upstream Caddyfile / docker-compose.yml changes from a newer crabcc
build, but never clobbers `.env`.

### Path B — Repo checkout

```bash
cd $(git rev-parse --show-toplevel)/install/ollama-stack
cp .env.example .env
./init-keys.sh
docker compose up -d --wait
```

Same files as Path A, just operated from the source tree. Useful when
you're hacking on the Compose recipe itself.

## API keys

Two distinct keys. Both 32-byte hex by default; rotate via
`init-keys.sh --rotate`.

| key | purpose | who sees it |
|---|---|---|
| `OLLAMA_API_KEY` | Bearer token Caddy enforces on `/api` + `/v1` | LiteLLM (server-to-server), direct curl tests |
| `LITELLM_MASTER_KEY` | Master key LiteLLM accepts on `/v1/*` and `/key/*` | end clients (humans + agents) |

The `init-keys.sh` script writes both into `.env` with mode 600.
Recommended: also persist a copy of `LITELLM_MASTER_KEY` to
`~/.crabcc.local.api-key` with `chmod 400`:

```bash
~/.crabcc/ollama-stack/init-keys.sh --quiet > ~/.crabcc.local.api-key
chmod 400 ~/.crabcc.local.api-key
```

Then in your shell rc:

```bash
export OLLAMA_API_KEY="$(cat ~/.crabcc.local.api-key)"
export OLLAMA_BASE_URL="http://localhost:4000"
```

`crabcc agent --backend ollama` reads both env vars.

## First model pull

Ollama starts empty. Pull a model into the running container:

```bash
docker compose -f ~/.crabcc/ollama-stack/docker-compose.yml exec ollama ollama pull qwen2.5-coder
```

`qwen2.5-coder` is the default for `crabcc agent --backend ollama`
(purpose-built for code; matches `litellm.config.yaml`'s `model_list`).
Other defaults: `llama3.2` for general chat, `nomic-embed-text` for
embeddings.

The `ollama_models` named volume persists models across `docker compose
down`. Only `down -v` wipes them.

## Verifying

Caddy enforces auth on `/api` and `/v1`:

```bash
# Unauth → 401
curl -i http://localhost:11435/api/tags

# Authed → JSON model list
source ~/.crabcc/ollama-stack/.env
curl -s -H "Authorization: Bearer $OLLAMA_API_KEY" http://localhost:11435/api/tags | jq .
```

LiteLLM speaks OpenAI:

```bash
curl -s http://localhost:4000/v1/chat/completions \
  -H "Authorization: Bearer $LITELLM_MASTER_KEY" \
  -H 'Content-Type: application/json' \
  -d '{
        "model": "ollama/qwen2.5-coder",
        "messages": [{"role": "user", "content": "ping"}]
      }' | jq -r '.choices[0].message.content'
```

For full e2e coverage including failure modes, run
[`MANUAL_TEST_CHECKLIST.md`](./MANUAL_TEST_CHECKLIST.md).

## Operating

| | |
|---|---|
| `crabcc ollama-stack up`     | `docker compose up -d --wait` |
| `crabcc ollama-stack status` | per-container JSON (image, status, health, ports, networks) |
| `crabcc ollama-stack logs litellm --tail 200` | tail any service |
| `crabcc ollama-stack pull`   | refresh upstream images |
| `crabcc ollama-stack down`   | stop containers, KEEP volumes (model cache) |
| `crabcc ollama-stack down --volumes` | stop + wipe volumes (re-pull on next up) |

`ccc setup --ollama-{up,down,status,pull,down-volumes}` are the
high-level shortcuts. Same operations, smaller surface (issue #74).

## Refreshing

```bash
crabcc upgrade --with-stack            # pull-only, read-only
crabcc upgrade --with-stack --apply    # pull + re-up; recreates services
                                       # whose image digest changed
```

Use `--apply` when a new crabcc release ships an updated Caddyfile or
LiteLLM config; bare `--with-stack` is enough when only upstream images
have new tags.

## OS-specific notes

### macOS

OrbStack (https://orbstack.dev) is preferred over Docker Desktop —
faster, no licensing seat. `crabcc` detects OrbStack via
`~/.orbstack/run/docker.sock` and adjusts the install hint
accordingly:

```bash
brew install orbstack
open -a OrbStack
```

### Linux

Docker Engine + the `compose` plugin:

```bash
# https://docs.docker.com/engine/install/
sudo usermod -aG docker $USER && newgrp docker
```

### WSL2

Docker Desktop's WSL2 backend works. Native Docker inside WSL also
works; pick whichever you're already running. The Compose recipe is
arch-agnostic — `linux/amd64` or `linux/arm64` images pull
automatically.

## Networking

Two networks:

- `stack` — internal to the Compose stack, joining `ollama` ↔ `caddy`
  ↔ `litellm`. Not joinable from outside.
- `crabcc-shared` — external bridge created once via
  [`install/init-shared-network.sh`](../init-shared-network.sh). The
  dev compose's `crabcc` service joins this so it can reach
  `litellm:4000` without the host port hop. Future BullMQ workers
  (issue #109) join here too.

## Out of scope

Documented as future work, not in this stack:

- TLS / mTLS termination at Caddy.
- Per-IP rate limiting.
- LiteLLM Postgres-backed virtual keys (file-based config is enough).
- Distributed deployment (Redis cluster mode for jobs, multi-region
  Caddy front).
- Non-Docker container runtimes (Podman, containerd).

## See also

- [`README.md`](./README.md) — quick-start
- [`MANUAL_TEST_CHECKLIST.md`](./MANUAL_TEST_CHECKLIST.md) — 12-section e2e gate
- Issue #105 (https://github.com/peterlodri-sec/crabcc/issues/105) — full design + acceptance criteria
- Issue #109 (https://github.com/peterlodri-sec/crabcc/issues/109) — BullMQ-backed jobs (sibling work, joins the same `crabcc-shared` network)
- Issue #112 (https://github.com/peterlodri-sec/crabcc/issues/112) — perf pass (PGO + SQLite PRAGMAs + targeted SIMD)
