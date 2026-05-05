"""Settings — every knob is an env var, validated once at startup."""

from __future__ import annotations

from pydantic import Field
from pydantic_settings import BaseSettings, SettingsConfigDict


class Settings(BaseSettings):
    """Service-wide configuration.

    All fields read from env vars with the ``CRABCC_HITL_`` prefix.
    The ``model_config`` block also reads a sibling ``.env`` file when
    present (handy for local dev outside Docker).
    """

    model_config = SettingsConfigDict(
        env_prefix="CRABCC_HITL_",
        env_file=".env",
        env_file_encoding="utf-8",
        extra="ignore",
    )

    # ───── HTTP server ─────
    host: str = "0.0.0.0"
    port: int = 9100

    # ───── Auth (between telegram-bot ↔ this service) ─────
    # Bearer token any caller must present on /chat. Telegram bot reads
    # the same value from its own env. None disables auth (tests only).
    api_token: str | None = None

    # ───── LiteLLM upstream ─────
    # The OpenAI-compatible base URL of the LiteLLM proxy. Inside
    # docker-compose this is the internal hostname; locally point at
    # http://localhost:4000.
    litellm_base_url: str = "http://litellm:4000"
    # LiteLLM master/virtual key — bearer used by the openai SDK.
    litellm_api_key: str = Field(default="sk-litellm-dev-key")
    # Model id as registered in `install/ollama-stack/litellm.config.yaml`.
    # Default to the cheapest Anthropic tool-call-capable model — the
    # full list is opus-4-7 / sonnet-4-6 / haiku-4-5 (Phase 0 doesn't
    # use tools yet, but the model still has to be tool-call-capable
    # for Phase 1 to land without re-config).
    model: str = "claude-haiku-4-5"

    # ───── Agent behaviour ─────
    # System prompt baseline. Kept minimal in Phase 0; Phase 1 will
    # extend with tool-use guidance.
    system_prompt: str = (
        "You are crabcc-helper, a code-search assistant embedded in a Telegram bot. "
        "Reply concisely. In later phases you will be given crabcc tool calls."
    )

    # Hard cap on the user-supplied task length. Telegram messages cap
    # at 4096 chars; we cap on top of that to keep prompt cost bounded.
    max_task_chars: int = 4_000

    # ───── Upstream tools (Phase 1+) ─────
    # crabcc MCP-HTTP base URL, e.g. http://crabcc-mcp:9090. When set,
    # the startup probe verifies reachability; Phase 1 will register
    # the MCP tools with the agent. Unset = tools disabled.
    mcp_base_url: str | None = None
    # Bearer token to send to the MCP service when ``mcp_base_url`` is
    # set. The MCP server is launched with ``crabcc --mcp-http addr
    # --auth-token <token>``.
    mcp_api_token: str | None = None

    # ───── Upstream connection-pool tuning ─────
    # Long-running async services that talk to a single upstream
    # benefit from a pre-warmed pool. Defaults are sane for ≤ 50
    # concurrent agent loops.
    httpx_max_connections: int = 32
    httpx_max_keepalive_connections: int = 16
    httpx_keepalive_expiry_s: float = 30.0
    httpx_connect_timeout_s: float = 5.0
    httpx_read_timeout_s: float = 60.0
    httpx_write_timeout_s: float = 30.0
    # HTTP/2 to upstream. LiteLLM speaks h2; multiplexing one TCP
    # connection across many parallel agent calls saves the handshake
    # tax and reduces head-of-line blocking. Toggle off only if a
    # corporate proxy mangles h2.
    httpx_http2: bool = True

    # ───── Startup probes ─────
    # Per-probe HTTP timeout. Lower than the request timeout because
    # a healthy upstream answers /health in milliseconds.
    probe_timeout_s: float = 3.0
    # How many extra attempts after the first. 3 retries × 2s base
    # backoff (capped at 30s) gives ~14s total grace — covers a
    # typical compose dependency cold-start.
    probe_startup_retries: int = 3
    probe_startup_retry_delay_s: float = 2.0
    # Master toggle. Tests set this to False; production never should.
    # (The lifespan still runs probes during /healthz when this is
    # True or False — this only gates the startup gate.)
    probe_startup_enabled: bool = True

    # ───── MCP server (Phase 0 exposure) ─────
    # Mounted at http://<host>:<mcp_port>/mcp. The HITL service exposes
    # its `chat` capability as an MCP tool so other host services
    # (Rust crabcc-mcp consumers, future agents) can call it through
    # the same protocol the rest of the workspace already speaks.
    mcp_enabled: bool = True
    mcp_port: int = 9101

    # ───── Logging ─────
    # `info` is the default for the service + uvicorn. Bump to `debug`
    # locally when chasing a problem; the root logger toggles per
    # request boundary fields without becoming firehose-y.
    log_level: str = "info"


def get_settings() -> Settings:
    """Singleton accessor used by FastAPI dependency injection.

    Re-reading env on every request would mean a missing var only
    surfaces under load, not at startup; this raises immediately.
    """
    return _settings_singleton


_settings_singleton = Settings()
