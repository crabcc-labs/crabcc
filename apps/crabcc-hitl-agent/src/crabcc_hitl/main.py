"""FastAPI entrypoint — Phase 0 round-trip endpoint.

Network + JSON are the dominant costs of this service. The wiring
below leans on the tightest mainstream Python idioms:

- pydantic v2's bytes path for both request decode and response
  encode (orjson-equivalent under the hood; FastAPI 0.115+ wires it
  automatically when ``response_model`` is set). The ``orjson`` dep
  stays in pyproject for non-FastAPI emit sites (probes, discovery).
- ``httpx[http2]`` with a pre-warmed connection pool, multiplexed h2
  to LiteLLM. See :func:`crabcc_hitl.llm.build_httpx_client`.
- ``uvloop`` + ``httptools`` (selected explicitly in the Dockerfile
  CMD) — C-backed event loop and HTTP parser.
- jemalloc preloaded in the runtime container (see Dockerfile) for
  lower fragmentation on long-running async workloads.
- A request-timing middleware that stamps ``X-Process-Time-Ms`` on
  every response and emits one info log per request.
- OTel auto-instrumentation for both inbound (FastAPI) and outbound
  (httpx) hops when ``OTEL_EXPORTER_OTLP_ENDPOINT`` is set.
- Startup probes that fail-fast on required-upstream-down — docker's
  restart policy then retries with backoff, beating "service comes up
  half-broken serving 5xx".
- An MCP server sibling that exposes ``chat`` as a tool over a
  separate port for in-cluster MCP consumers.
"""

from __future__ import annotations

import logging
import secrets
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from typing import Annotated

from fastapi import Depends, FastAPI, Header, HTTPException, Request, status
from pydantic import BaseModel, Field

from . import __version__
from .llm import HitlAgent, build_httpx_client
from .mcp_server import build_mcp, probe_mcp_started, run_mcp
from .probes import (
    ProbeConfig,
    ProbeResult,
    StartupCheckFailed,
    run_probes,
    run_startup_probes,
)
from .service_discovery import announce, maybe_register_redis
from .settings import Settings, get_settings
from .telemetry import init_telemetry, instrument_fastapi

logger = logging.getLogger(__name__)


def _setup_logging(level: str) -> None:
    """Tiny dictConfig — single console handler, plain text, level-driven.

    Idempotent: re-applies the same handler on each call so test
    reloads don't fan out to N handlers.
    """
    import sys

    root = logging.getLogger()
    root.setLevel(level.upper())
    # Drop all existing handlers — uvicorn installs its own; we want
    # a single line format across our logs and uvicorn's.
    for h in list(root.handlers):
        root.removeHandler(h)
    handler = logging.StreamHandler(stream=sys.stderr)
    handler.setFormatter(
        logging.Formatter(
            fmt="%(asctime)s %(levelname)-5s %(name)s %(message)s",
            datefmt="%Y-%m-%dT%H:%M:%S",
        )
    )
    root.addHandler(handler)


# JSON serialization note:
# FastAPI 0.115+ serializes responses via pydantic's bytes path
# (orjson-equivalent under the hood) when a `response_model` or return
# annotation is set, which is the case for every endpoint here.
# `request.json()` parses incoming bodies via pydantic's
# `model_validate_json`, which in pydantic v2 uses an internal Rust
# parser that beats stdlib `json` and matches orjson on these tiny
# bodies. So no custom Request/Response class is needed — the orjson
# dep stays in pyproject for downstream callers (probes / discovery
# emit JSON via orjson directly).


# ───── Schemas ──────────────────────────────────────────────────────────


class ChatRequest(BaseModel):
    """Body of POST /chat."""

    task: str = Field(..., min_length=1, description="The user's prompt verbatim.")
    # Reserved for Phase 1: lets the bot pin a session id so multi-turn
    # state survives across messages. Phase 0 ignores it.
    session_id: str | None = Field(default=None, description="Reserved (Phase 1).")


class ChatResponse(BaseModel):
    reply: str
    model: str


class ProbeView(BaseModel):
    name: str
    status: str
    latency_ms: int
    detail: str | None = None
    required: bool


class HealthResponse(BaseModel):
    status: str
    version: str
    checks: list[ProbeView]


# ───── App lifespan: startup probes, build the agent, tear down ─────


def _probe_cfg(s: Settings) -> ProbeConfig:
    return ProbeConfig(
        litellm_base_url=s.litellm_base_url,
        mcp_base_url=s.mcp_base_url,
        timeout_s=s.probe_timeout_s,
        startup_retries=s.probe_startup_retries,
        startup_retry_delay_s=s.probe_startup_retry_delay_s,
    )


@asynccontextmanager
async def lifespan(app: FastAPI) -> AsyncIterator[None]:
    import asyncio

    settings = get_settings()
    _setup_logging(settings.log_level)
    logger.debug("settings loaded", extra={"settings": settings.model_dump(exclude={"api_token"})})

    # OTel goes up first so the httpx client we build next gets traced.
    telemetry_active = init_telemetry(
        service_name="crabcc-hitl-agent",
        service_version=__version__,
    )
    if telemetry_active:
        instrument_fastapi(app)

    http_client = build_httpx_client(settings)
    app.state.http_client = http_client
    app.state.settings = settings

    # Run startup probes — required failures abort the process so
    # docker's restart policy retries with backoff. The
    # `probe_startup_enabled` flag exists for tests + emergency
    # bring-up when the upstream is known-down.
    cfg = _probe_cfg(settings)
    if settings.probe_startup_enabled:
        try:
            startup_results = await run_startup_probes(http_client, cfg)
        except StartupCheckFailed:
            await http_client.aclose()
            raise
    else:
        logger.warning("startup probes disabled (probe_startup_enabled=False)")
        startup_results = []
    app.state.last_probes = startup_results

    # Agent uses the now-warm connection pool.
    agent = HitlAgent(settings, http_client)
    app.state.agent = agent

    # Service-discovery: announce + best-effort Redis publish.
    endpoints = announce(http_port=settings.port, mcp_port=settings.mcp_port)
    await maybe_register_redis(endpoints)

    # MCP server runs as a sibling background task — same agent, same
    # httpx pool, separate transport. Bind early so the bot/other
    # clients can call us via either REST or MCP from the moment we
    # advertise readiness. We then probe the listener with a TCP
    # connect to catch "SDK swallowed a bind error" kinds of failures.
    mcp_task: asyncio.Task[None] | None = None
    if settings.mcp_enabled:
        mcp = build_mcp(agent, port=settings.mcp_port)
        app.state.mcp = mcp
        mcp_task = asyncio.create_task(run_mcp(mcp), name="crabcc-hitl-mcp")
        if not await probe_mcp_started(host="127.0.0.1", port=settings.mcp_port):
            # Non-fatal: REST API still works without MCP. Log loud so
            # the operator sees it; /healthz reflects degraded state
            # via the future MCP-self-probe (Phase 1 follow-up).
            logger.warning(
                "MCP server did not become reachable; service degraded but continuing",
                extra={"mcp_port": settings.mcp_port},
            )

    logger.info(
        "crabcc-hitl-agent ready",
        extra={
            "version": __version__,
            "http_port": settings.port,
            "mcp_port": settings.mcp_port if settings.mcp_enabled else None,
            "litellm_base_url": settings.litellm_base_url,
            "model": settings.model,
            "mcp_base_url": settings.mcp_base_url or "(unset)",
            "auth_required": settings.api_token is not None,
            "h2": settings.httpx_http2,
            "otel": telemetry_active,
        },
    )
    try:
        yield
    finally:
        logger.info("crabcc-hitl-agent shutting down")
        if mcp_task is not None:
            mcp_task.cancel()
            try:
                await mcp_task
            except (asyncio.CancelledError, Exception) as e:
                logger.debug("mcp task end", extra={"err": str(e)})
        await agent.aclose()
        await http_client.aclose()


app = FastAPI(
    title="crabcc-hitl-agent",
    version="0.1.0",
    description=(
        "Human-in-the-loop agent service for crabcc. Phase 0: bare "
        "round-trip via LiteLLM. Phase 1+: tool calls and approval flow."
    ),
    lifespan=lifespan,
)


# ───── Request-timing middleware ─────
#
# Stamps every response with `X-Process-Time-Ms` for downstream log
# correlation + emits a single info log per request (route, status,
# wall-time, client). Cheap — `time.perf_counter` is a syscall-free
# monotonic on macOS/Linux. Skipping `/healthz` keeps the docker
# probe noise out of the log.


@app.middleware("http")
async def _timing_middleware(request: Request, call_next):  # type: ignore[no-untyped-def]
    import time

    started = time.perf_counter()
    response = await call_next(request)
    elapsed_ms = (time.perf_counter() - started) * 1000.0
    response.headers["X-Process-Time-Ms"] = f"{elapsed_ms:.1f}"
    if request.url.path != "/healthz":
        logger.info(
            "request",
            extra={
                "method": request.method,
                "path": request.url.path,
                "status": response.status_code,
                "elapsed_ms": round(elapsed_ms, 1),
                "client": request.client.host if request.client else None,
            },
        )
    return response


# ───── Auth dependency ─────


def _verify_token(
    settings: Annotated[Settings, Depends(get_settings)],
    authorization: str | None = Header(default=None),
) -> None:
    """Bearer-token check.

    No-op when ``CRABCC_HITL_API_TOKEN`` is unset (tests, local dev).
    Constant-time compare via :func:`secrets.compare_digest` so a
    timing oracle can't probe the token byte-by-byte.
    """
    if settings.api_token is None:
        return
    if authorization is None or not authorization.startswith("Bearer "):
        raise HTTPException(
            status_code=status.HTTP_401_UNAUTHORIZED,
            detail="missing bearer token",
            headers={"WWW-Authenticate": "Bearer"},
        )
    presented = authorization.removeprefix("Bearer ").strip()
    if not secrets.compare_digest(presented, settings.api_token):
        raise HTTPException(
            status_code=status.HTTP_401_UNAUTHORIZED,
            detail="bad bearer token",
            headers={"WWW-Authenticate": "Bearer"},
        )


def _to_view(r: ProbeResult) -> ProbeView:
    return ProbeView(
        name=r.name,
        status=r.status,
        latency_ms=r.latency_ms,
        detail=r.detail,
        required=r.required,
    )


# ───── Endpoints ─────


@app.get("/healthz", response_model=HealthResponse)
async def healthz() -> HealthResponse:
    """Liveness/readiness probe.

    Re-runs every probe live so the docker HEALTHCHECK reports current
    state, not the cached startup view. Returns ``status="degraded"``
    when an optional probe is failing (process is up but tools may be
    unavailable); ``"fail"`` when a required probe is failing (rare
    post-startup, would imply the upstream just dropped).
    """
    settings: Settings = app.state.settings
    http_client = app.state.http_client
    results = await run_probes(http_client, _probe_cfg(settings))
    overall: str
    if any(r.required and not r.passed for r in results):
        overall = "fail"
    elif any(not r.passed for r in results):
        overall = "degraded"
    else:
        overall = "ok"
    logger.debug(
        "healthz probed",
        extra={
            "overall": overall,
            "checks": [{"name": r.name, "status": r.status, "ms": r.latency_ms} for r in results],
        },
    )
    return HealthResponse(
        status=overall,
        version=__version__,
        checks=[_to_view(r) for r in results],
    )


@app.post("/chat", response_model=ChatResponse, dependencies=[Depends(_verify_token)])
async def chat(req: ChatRequest) -> ChatResponse:
    """Round-trip a user prompt through the LiteLLM-backed agent."""
    settings: Settings = app.state.settings
    agent: HitlAgent = app.state.agent
    logger.debug(
        "chat: incoming",
        extra={"task_len": len(req.task), "session_id": req.session_id},
    )
    reply = await agent.chat(req.task)
    logger.info(
        "chat: completed",
        extra={
            "model": settings.model,
            "task_len": len(req.task),
            "reply_len": len(reply),
        },
    )
    return ChatResponse(reply=reply, model=settings.model)
