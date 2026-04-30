# crabcc Ollama auth stack

Three-service Docker Compose recipe putting Caddy (Bearer-auth reverse proxy)
and LiteLLM (OpenAI-compatible front) in front of Ollama. Backs issue #105.

## Why

Ollama has no native API-key auth. The moment it's exposed beyond loopback
(shared dev box, LAN, hosted runner) the endpoint is open compute. Upstream's
recommendation is a reverse proxy; this stack pre-bakes that with the
OpenAI-compatible front bolted on.

## Quick start

```bash
# 0. Create the shared network once (idempotent).
install/init-shared-network.sh

cd install/ollama-stack
cp .env.example .env

# generate two random tokens
python3 -c "import secrets; print('OLLAMA_API_KEY=' + secrets.token_hex(32))"     >> .env
python3 -c "import secrets; print('LITELLM_MASTER_KEY=sk-' + secrets.token_hex(32))" >> .env
# then trim the placeholder lines

docker compose up -d --wait
docker compose ps
```

You'll have:

| port  | service          | auth                                 |
| ----- | ---------------- | ------------------------------------ |
| 11435 | Caddy → Ollama   | `Authorization: Bearer $OLLAMA_API_KEY`     |
| 4000  | LiteLLM proxy    | `Authorization: Bearer $LITELLM_MASTER_KEY` |

The Ollama container itself is **never** exposed to the host.

## First model pull

Ollama starts empty. Pull a model into the container:

```bash
docker compose exec ollama ollama pull llama3.2
docker compose exec ollama ollama pull qwen2.5-coder
docker compose exec ollama ollama pull nomic-embed-text
```

The `ollama_models` named volume persists models across `docker compose down`
(only `down -v` wipes them).

## Smoke tests

Caddy enforces auth on `/api` and `/v1`:

```bash
# unauth → 401
curl -i http://localhost:11435/api/tags

# authed → JSON model list
curl -s -H "Authorization: Bearer $OLLAMA_API_KEY" http://localhost:11435/api/tags
```

LiteLLM speaks OpenAI:

```bash
curl -s http://localhost:4000/v1/chat/completions \
  -H "Authorization: Bearer $LITELLM_MASTER_KEY" \
  -H 'Content-Type: application/json' \
  -d '{
        "model": "ollama/llama3.2",
        "messages": [{"role": "user", "content": "ping"}]
      }'
```

## Operating

| | |
|---|---|
| `docker compose up -d --wait` | bring stack up, block until healthy |
| `docker compose down`         | stop containers, keep volumes (model cache stays) |
| `docker compose down -v`      | stop and wipe volumes (re-pull models on next up) |
| `docker compose ps`           | service status |
| `docker compose logs -f <svc>`| follow logs for one service |
| `docker compose pull`         | refresh upstream images (call before next `up`) |

## Wiring crabcc

Set in your shell or `.envrc`:

```bash
export OLLAMA_BASE_URL=http://localhost:4000
export OLLAMA_API_KEY="$LITELLM_MASTER_KEY"   # crabcc talks to LiteLLM, not Caddy
```

Then `crabcc agent --backend ollama --run "..."` will route through the stack.

> Phase 2+ of issue #105 wires `crabcc ollama-stack {up,down,status,logs,pull}`
> as thin wrappers over these `docker compose` calls, with auto-spinup from
> `crabcc agent` when the backend is ollama. This README documents the manual
> path; the CLI sugar lands in subsequent commits.

## Out of scope

- TLS / mTLS termination at Caddy.
- Rate limiting / per-IP quotas.
- LiteLLM Postgres-backed virtual keys (commented stub in `litellm.config.yaml`).
- Non-Docker container runtimes.

See issue #105 for the full design + acceptance criteria.
