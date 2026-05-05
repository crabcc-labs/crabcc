# crabcc-hitl-agent

Human-in-the-loop agent service. Sits between the Rust Telegram bot and the LiteLLM proxy. **Phase 3** adds a per-arg auto-approve allowlist, an in-process audit ring buffer, and a local-dev console (`/devapp/`) so the whole flow can be driven from a browser without Telegram.

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

Phase 1 added a `tools/` package registering the crabcc MCP HTTP surface (sym/refs/callers/files/outline/fuzzy/memory.*) plus `fetch_url`. Phase 2 gates each side-effecting tool behind a Telegram approval prompt and exposes a Mini App for queue management.

## Endpoints

| Method | Path | Auth | Purpose |
|---|---|---|---|
| `GET`  | `/healthz` | none | Liveness probe (k8s-shaped). |
| `POST` | `/chat`    | bearer | Single round-trip: `{task, tg_chat_id?}` → `{reply, model}`. |
| `POST` | `/approval/respond` | bearer | Bot forwards inline-button taps. |
| `GET`  | `/approval/list`    | bearer | Snapshot of pending approvals. |
| `GET`  | `/approval/audit`   | bearer | Recent gate decisions, newest first. |
| `GET`  | `/webapp/*`         | initData | Telegram Mini App static bundle. |
| `GET`  | `/webapp/api/approvals` | initData | Mini App approvals listing. |
| `POST` | `/webapp/api/respond`   | initData | Mini App approve/deny. |
| `GET`  | `/webapp/api/audit`     | initData | Mini App audit listing. |
| `GET`  | `/devapp/*`         | bearer (in page) | Local-dev browser console. **Loopback only.** |

Body for `POST /chat`:

```json
{ "task": "find Store callers", "tg_chat_id": 5875395828 }
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
| `CRABCC_HITL_TELEGRAM_BOT_TOKEN` | _(unset)_ | Bot token used to **send** approval prompts and validate Mini App initData. Empty disables the gate; required tools fail closed. |
| `CRABCC_HITL_TELEGRAM_OWNER_CHAT_ID` | _(unset)_ | Fallback chat id for approvals when `/chat` carried no `tg_chat_id`. |
| `CRABCC_HITL_APPROVAL_TIMEOUT_S` | `120` | Per-tool wait time. Times out into a synthetic deny. |
| `CRABCC_HITL_APPROVAL_REQUIRED_TOOLS` | `memory_remember,fetch_url` | Comma-separated list of tools that always need approval. Read-only tools auto-run. |
| `CRABCC_HITL_APPROVAL_AUTO_PATTERNS` | _(unset)_ | Per-arg allowlist that bypasses the prompt. Comma-separated `tool:arg=glob`. Example: `fetch_url:url=https://github.com/**`. |
| `CRABCC_HITL_AUDIT_CAPACITY` | `200` | Ring-buffer size for `/approval/audit`. In-memory only. |

## Local dev

The fastest path is the repo-root Taskfile target — brings up the full stack and exposes a browser console at `http://127.0.0.1:9100/devapp/`.

```bash
task hitl:up        # generates .env if missing; starts ollama+caddy+litellm+hitl
task hitl:open      # opens the dev console in your browser
task hitl:logs      # tails hitl logs (SERVICE=litellm/caddy/ollama for others)
task hitl:test      # ruff + ruff format + mypy strict + pytest
task hitl:down      # tear down (volumes preserved)
```

The dev console is a single static page that authenticates with the bearer token (paste once, persists in `localStorage`), sends `/chat`, polls `/approval/list`, posts `/approval/respond`, and renders `/approval/audit` — the full HITL flow without Telegram. Loopback-only by design; the cloudflared tunnel only fronts `/webapp`.

Manual setup (no Taskfile):

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

## cloudflared / Telegram Mini App

The bot ↔ HITL service path **does not need cloudflared** — both run inside the `crabcc-shared` docker network and reach each other by service DNS. Bearer-token auth is the only boundary check.

Telegram Mini Apps require a publicly-reachable HTTPS URL (BotFather rejects `http://` and private hostnames). The compose stack ships a `cloudflared` service behind a `tunnel` profile:

```bash
# 1. Cloudflare Zero Trust → Networks → Tunnels → Create.
# 2. Pick "Cloudflared" connector; copy the token.
# 3. Add a public hostname pointing at `hitl:9100`, path `/webapp`.
# 4. Drop the token in install/ollama-stack/.env:
echo "CLOUDFLARED_TUNNEL_TOKEN=<token>" >> install/ollama-stack/.env

# 5. Bring up the tunnel alongside the rest of the stack:
docker compose --profile tunnel up -d
```

Configure the bot once: `BotFather → /setmenubutton` (or `/newbot` Mini App URL) with the public `https://<hostname>/webapp/` URL. The internal `/chat` endpoint stays loopback-only — only `/webapp/*` is exposed.

Mini App auth is `X-Telegram-Init-Data`-validated (HMAC-SHA256 over the bot token). Bearer tokens are **not** accepted on `/webapp/api/*`.

## Layout

```
apps/crabcc-hitl-agent/
├── pyproject.toml             # uv-managed, ruff/mypy/pytest configured
├── Dockerfile                 # multi-stage; linux/arm64 first
├── docker-compose.yml         # joins crabcc-shared
├── README.md
├── src/
│   └── crabcc_hitl/
│       ├── __init__.py
│       ├── _types.py          # shared TypedDicts (Telegram wire shapes)
│       ├── main.py            # FastAPI app: /healthz, /chat, /approval/*, /webapp/*
│       ├── settings.py        # pydantic-settings, env-driven
│       ├── llm.py             # OpenAI Agents SDK wrapper → LiteLLM
│       ├── approvals.py       # PendingApprovals registry + ContextVar
│       ├── telegram_client.py # Bot REST client + initData validator
│       ├── tool_gate.py       # gated() decorator wrapping each tool
│       ├── policy.py          # per-arg auto-approve allowlist (Phase 3)
│       ├── audit.py           # in-process decision ring buffer (Phase 3)
│       ├── tools/             # crabcc + memory + fetch tool registry
│       ├── webapp/            # Mini App static bundle (Telegram)
│       └── devapp/            # local-dev browser console (bearer auth)
└── tests/
    ├── test_chat.py           # mocked Runner.run; auth + clip tests
    ├── test_fetch_url.py      # markitdown tool wrapper
    ├── test_phase1_tools.py   # crabcc + memory wrapper tests
    ├── test_phase2_approval.py # gate + registry + initData
    └── test_phase3_policy_audit.py # policy parser + audit + e2e
```
