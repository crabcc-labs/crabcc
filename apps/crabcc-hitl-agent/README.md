# crabcc-hitl-agent

Human-in-the-loop agent service. Sits between the Rust Telegram bot and the LiteLLM proxy. **Phase 0** ships the round-trip wiring — no tools, no approval flow yet.

## Architecture

```
Telegram User
    ↕ long-poll
crabcc-telegram (Rust)               ← apps/crabcc-telegram
    ↕ HTTP, "crabcc-shared" docker net, bearer auth
crabcc-hitl-agent (Python, this)     ← apps/crabcc-hitl-agent
    ↕ OpenAI-compatible API
LiteLLM proxy :4000                  ← install/ollama-stack
    ↕
LLM (Claude tool-call models / Ollama fallback)
```

Phase 1 will add a `tools/` package that registers the crabcc MCP HTTP surface (sym/refs/callers/files/outline/fuzzy/memory.*) as Agent tools. Phase 2 wires the human-approval flow (inline-button "approve / reject" via the bot before any tool runs).

## Endpoints

| Method | Path | Auth | Purpose |
|---|---|---|---|
| `GET`  | `/healthz` | none | Liveness probe (k8s-shaped). |
| `POST` | `/chat`    | bearer | Single round-trip: `{task}` → model → `{reply, model}`. |
| _reserved_ | `/webapp/*` | (future) | Telegram Mini App surface (Phase 2). |

Body for `POST /chat`:

```json
{ "task": "what does crabcc-godfather do?", "session_id": null }
```

Response:

```json
{ "reply": "...", "model": "claude-haiku-4-5" }
```

## Configuration

Every knob is an env var with the `CRABCC_HITL_` prefix.

| Var | Default | Notes |
|---|---|---|
| `CRABCC_HITL_HOST` | `0.0.0.0` | Bind address inside the container. |
| `CRABCC_HITL_PORT` | `9100` | HTTP port. |
| `CRABCC_HITL_API_TOKEN` | _(unset)_ | Bearer token the bot must present. Unset = auth disabled (tests / local dev only). |
| `CRABCC_HITL_LITELLM_BASE_URL` | `http://litellm:4000` | LiteLLM proxy URL. |
| `CRABCC_HITL_LITELLM_API_KEY` | `sk-litellm-dev-key` | LiteLLM master / virtual key. |
| `CRABCC_HITL_MODEL` | `claude-haiku-4-5` | Model id as registered in `install/ollama-stack/litellm.config.yaml`. |
| `CRABCC_HITL_MAX_TASK_CHARS` | `4000` | Hard cap on user prompt length. Truncates with a clear suffix. |

## Local dev

```bash
# 1. Install (uv recommended; pip works too).
cd apps/crabcc-hitl-agent
uv venv
uv pip install -e .[dev]

# 2. Test (mocks LiteLLM — no network).
uv run pytest

# 3. Run against a local LiteLLM (start the ollama-stack first).
export CRABCC_HITL_LITELLM_API_KEY="$(cat ~/.crabcc/litellm.master.key)"
uv run uvicorn crabcc_hitl.main:app --reload --port 9100
```

## Container

Build:

```bash
docker buildx build -f apps/crabcc-hitl-agent/Dockerfile \
    --platform linux/arm64 \
    -t ghcr.io/peterlodri-sec/crabcc-hitl-agent:dev --load \
    apps/crabcc-hitl-agent
```

Apple-native runtime (`container` CLI, macOS 15+) — same image, OCI-compatible:

```bash
container run \
    --image ghcr.io/peterlodri-sec/crabcc-hitl-agent:dev \
    --env-file apps/crabcc-hitl-agent/.env \
    --port 9100:9100
```

Compose (joins the `crabcc-shared` network created by the ollama-stack):

```bash
cd apps/crabcc-hitl-agent
cp .env.example .env  # see §Configuration
docker compose up -d
docker compose logs -f hitl
```

## §5 cloudflared / Telegram Mini App

The bot ↔ HITL service path **does not need cloudflared** — both run inside the `crabcc-shared` docker network and reach each other by service DNS. Bearer-token auth is the only boundary check.

Telegram Mini Apps require a publicly-reachable HTTPS URL (BotFather rejects `http://` and private hostnames). When Phase 2 lands a webapp, expose only the `/webapp/*` path through cloudflared:

```yaml
# ~/.cloudflared/config.yml — bot-specific tunnel.
tunnel: <tunnel-id>
ingress:
  - hostname: hitl.<your-domain>
    path: /webapp/*
    service: http://127.0.0.1:9100
  # Reject everything else publicly. /chat must NEVER be exposed —
  # auth is bearer-only and the bot is the sole legitimate caller.
  - service: http_status:404
```

Then `BotFather → /setmenubutton` (or the `/newbot` Mini App URL prompt) takes the public `https://hitl.<your-domain>/webapp/...` URL. The internal `/chat` endpoint stays loopback-only.

## Layout

```
apps/crabcc-hitl-agent/
├── pyproject.toml           # uv-managed, ruff/mypy/pytest configured
├── Dockerfile               # multi-stage; linux/arm64 first
├── docker-compose.yml       # joins crabcc-shared
├── README.md
├── src/
│   └── crabcc_hitl/
│       ├── __init__.py
│       ├── main.py          # FastAPI app, /healthz + /chat
│       ├── settings.py      # pydantic-settings, env-driven
│       └── llm.py           # OpenAI Agents SDK wrapper → LiteLLM
└── tests/
    └── test_chat.py         # mocked Runner.run; auth + clip tests
```
