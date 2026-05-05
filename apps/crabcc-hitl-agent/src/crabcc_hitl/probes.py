"""Liveness probes for upstream services.

Run during the FastAPI ``lifespan`` and again on every ``/healthz``
hit. Each probe is small and async; failures attach a human-readable
reason for the operator. Required probes failing at startup raise
``StartupCheckFailed`` so the container exits non-zero — docker's
``restart: unless-stopped`` then retries with backoff (better than the
service coming up half-broken and serving 5xx to the bot).
"""

from __future__ import annotations

import logging
import time
from dataclasses import dataclass
from typing import Final, Literal

import httpx

logger = logging.getLogger(__name__)

# Status strings are used in JSON responses; keep them stable.
ProbeStatus = Literal["ok", "degraded", "fail", "skipped"]


@dataclass(frozen=True, slots=True)
class ProbeResult:
    """Outcome of a single probe."""

    name: str
    status: ProbeStatus
    latency_ms: int
    detail: str | None = None
    required: bool = True

    @property
    def passed(self) -> bool:
        return self.status in ("ok", "degraded", "skipped")


class StartupCheckFailed(RuntimeError):
    """Raised when a required probe failed at startup.

    Caught by the FastAPI ``lifespan`` and re-raised so uvicorn exits.
    Compose's ``restart: unless-stopped`` will retry, giving the
    upstream a chance to come up.
    """


# ───── Probe primitives ────────────────────────────────────────────────


async def probe_litellm(
    client: httpx.AsyncClient,
    base_url: str,
    timeout_s: float,
) -> ProbeResult:
    """Hit LiteLLM's `/health` endpoint.

    LiteLLM exposes /health on the same port as the OpenAI-compat
    surface. Returns 200 + JSON body when healthy. We don't validate
    the body shape — just a connect + 2xx is enough to know the proxy
    is breathing.
    """
    started = time.perf_counter()
    try:
        resp = await client.get(f"{base_url.rstrip('/')}/health", timeout=timeout_s)
    except httpx.HTTPError as e:
        elapsed_ms = int((time.perf_counter() - started) * 1000)
        return ProbeResult(
            name="litellm",
            status="fail",
            latency_ms=elapsed_ms,
            detail=f"connect failed: {e.__class__.__name__}: {e}",
        )
    elapsed_ms = int((time.perf_counter() - started) * 1000)
    if 200 <= resp.status_code < 300:
        return ProbeResult(name="litellm", status="ok", latency_ms=elapsed_ms)
    return ProbeResult(
        name="litellm",
        status="fail",
        latency_ms=elapsed_ms,
        detail=f"http {resp.status_code}",
    )


async def probe_crabcc_mcp(
    client: httpx.AsyncClient,
    base_url: str | None,
    timeout_s: float,
) -> ProbeResult:
    """Hit the crabcc MCP-HTTP `/healthz` endpoint when configured.

    Phase 0 doesn't call crabcc tools yet — but Phase 1 will. By
    probing here we surface "tools will be unavailable" *before* a
    user `/agent` request lands. When ``base_url`` is unset (Phase 0
    default) the probe reports ``skipped`` and is *not* required.
    """
    if not base_url:
        return ProbeResult(
            name="crabcc_mcp",
            status="skipped",
            latency_ms=0,
            detail="CRABCC_HITL_MCP_BASE_URL not set",
            required=False,
        )
    started = time.perf_counter()
    try:
        resp = await client.get(f"{base_url.rstrip('/')}/healthz", timeout=timeout_s)
    except httpx.HTTPError as e:
        elapsed_ms = int((time.perf_counter() - started) * 1000)
        return ProbeResult(
            name="crabcc_mcp",
            status="fail",
            latency_ms=elapsed_ms,
            detail=f"connect failed: {e.__class__.__name__}: {e}",
            required=False,  # Phase 0: not required; flip to True in Phase 1.
        )
    elapsed_ms = int((time.perf_counter() - started) * 1000)
    if 200 <= resp.status_code < 300:
        return ProbeResult(
            name="crabcc_mcp",
            status="ok",
            latency_ms=elapsed_ms,
            required=False,  # Phase 0: not required; flip to True in Phase 1.
        )
    return ProbeResult(
        name="crabcc_mcp",
        status="fail",
        latency_ms=elapsed_ms,
        detail=f"http {resp.status_code}",
        required=False,
    )


# ───── Orchestrator ────────────────────────────────────────────────────


@dataclass(frozen=True, slots=True)
class ProbeConfig:
    litellm_base_url: str
    mcp_base_url: str | None
    timeout_s: float
    startup_retries: int
    startup_retry_delay_s: float


_BACKOFF_CAP_S: Final[float] = 30.0


async def run_probes(
    client: httpx.AsyncClient,
    cfg: ProbeConfig,
) -> list[ProbeResult]:
    """Run every configured probe once. Used by ``/healthz`` live."""
    results = [
        await probe_litellm(client, cfg.litellm_base_url, cfg.timeout_s),
        await probe_crabcc_mcp(client, cfg.mcp_base_url, cfg.timeout_s),
    ]
    return results


async def run_startup_probes(
    client: httpx.AsyncClient,
    cfg: ProbeConfig,
) -> list[ProbeResult]:
    """Run probes with retry; raise on required-probe failure.

    Backoff: ``retry_delay_s * 2^attempt``, capped at 30s. Default
    config (3 retries × 2s base) gives ~14s of grace, which covers a
    typical compose dependency cold-start.
    """
    last_results: list[ProbeResult] = []
    for attempt in range(cfg.startup_retries + 1):
        last_results = await run_probes(client, cfg)
        required_failures = [r for r in last_results if r.required and not r.passed]
        if not required_failures:
            for r in last_results:
                logger.info(
                    "startup probe ok",
                    extra={
                        "probe": r.name,
                        "status": r.status,
                        "latency_ms": r.latency_ms,
                        "detail": r.detail,
                    },
                )
            return last_results
        # Log + sleep before retrying.
        for r in required_failures:
            logger.warning(
                "startup probe failed (will retry)",
                extra={
                    "probe": r.name,
                    "status": r.status,
                    "latency_ms": r.latency_ms,
                    "detail": r.detail,
                    "attempt": attempt + 1,
                    "max_attempts": cfg.startup_retries + 1,
                },
            )
        if attempt < cfg.startup_retries:
            import asyncio

            delay = min(_BACKOFF_CAP_S, cfg.startup_retry_delay_s * (2**attempt))
            await asyncio.sleep(delay)
    # Out of retries — abort.
    failures = "; ".join(
        f"{r.name}: {r.detail or r.status}" for r in last_results if r.required and not r.passed
    )
    raise StartupCheckFailed(f"required probes failed after retries: {failures}")
