"""Service-discovery hook for the HITL agent.

Aligns with the workspace's existing service-discovery convention
(``crates/crabcc-core/src/service_discovery.rs`` — env-var-first;
compose populates the defaults under ``CRABCC_COMPOSE=1``).

Discovery in this workspace is **consumer-driven**: services don't
register actively — instead each consumer reads a known env var
(``REDIS_URL``, ``OTEL_EXPORTER_OTLP_ENDPOINT``, etc.) and the
``service_discovery.rs`` enumerator probes the union of those URLs
to surface health.

So this module's role is bounded:

1. **Logs the canonical service URLs at startup** so an operator can
   `docker compose logs hitl | grep -i 'discovery'` and see exactly
   what URL the bot / other consumers should use.

2. **Exposes :data:`DEFAULT_HITL_HTTP_URL` / :data:`DEFAULT_HITL_MCP_URL`**
   as the values the Rust side will look for under ``CRABCC_HITL_URL``
   / ``CRABCC_HITL_MCP_URL``. When ``service_discovery.rs`` is updated
   (tracked in #511's sibling work for the MCP variant of #204), the
   shape stays compatible.

3. **Optional Redis registration** — when
   ``CRABCC_HITL_DISCOVERY_REDIS_URL`` is set the service publishes its
   listening URL to a known key (``crabcc:services:hitl``). Any
   consumer that prefers Redis lookup over env-var lookup can read it
   there. No-op when unset.
"""

from __future__ import annotations

import logging
import os
import socket
from dataclasses import dataclass

logger = logging.getLogger(__name__)


# Defaults assume the compose-network shape: service name `hitl` on the
# `crabcc-shared` bridge, port 9100 for HTTP and 9101 for MCP. Docker
# DNS resolves the service name; the Rust SD's `h!("rotel", "127.0.0.1")`
# helper takes the same compose-vs-local fallback approach.
DEFAULT_HITL_HTTP_URL = "http://hitl:9100"
DEFAULT_HITL_MCP_URL = "http://hitl:9101/mcp"


@dataclass(frozen=True, slots=True)
class HitlEndpoints:
    http_url: str
    mcp_url: str
    hostname: str


def announce(http_port: int, mcp_port: int) -> HitlEndpoints:
    """Emit a startup log line with the canonical URLs.

    The log shape (``key=value``) matches what
    ``service_discovery.rs`` parses for its own startup announce, so
    the same log-collection rules pick both up.
    """
    hostname = socket.gethostname()
    # When running inside compose, the canonical URL uses the service
    # name (``hitl``) — that's how peer containers reach us. Outside
    # compose, fall back to ``127.0.0.1``.
    in_compose = os.environ.get("CRABCC_COMPOSE", "").lower() in ("1", "true", "yes")
    base = "hitl" if in_compose else "127.0.0.1"
    endpoints = HitlEndpoints(
        http_url=f"http://{base}:{http_port}",
        mcp_url=f"http://{base}:{mcp_port}/mcp",
        hostname=hostname,
    )
    logger.info(
        "service-discovery announce",
        extra={
            "service": "crabcc-hitl-agent",
            "http_url": endpoints.http_url,
            "mcp_url": endpoints.mcp_url,
            "hostname": hostname,
            "compose": in_compose,
        },
    )
    return endpoints


async def maybe_register_redis(endpoints: HitlEndpoints) -> bool:
    """Best-effort publish to the Redis service catalog.

    Active only when ``CRABCC_HITL_DISCOVERY_REDIS_URL`` is set;
    otherwise this is a no-op. Failures log a warning and are
    swallowed — Redis-down must not gate process startup. Returns
    ``True`` on successful write.

    Storage shape (matches what the Rust SD reads / will read):

    - Key: ``crabcc:services:hitl``
    - Value: JSON ``{"http_url": ..., "mcp_url": ..., "hostname": ...}``
    """
    redis_url = os.environ.get("CRABCC_HITL_DISCOVERY_REDIS_URL")
    if not redis_url:
        return False
    try:
        # Lazy import — `redis` isn't a hard dep; skip when unused.
        import orjson  # already a project dep
        import redis.asyncio as aioredis  # noqa: F401  ensure module is importable
    except ImportError:
        logger.warning("discovery redis url set but `redis` package not installed; skipping")
        return False
    try:
        from redis.asyncio import Redis

        client = Redis.from_url(redis_url, encoding="utf-8", decode_responses=False)
        payload = orjson.dumps(
            {
                "http_url": endpoints.http_url,
                "mcp_url": endpoints.mcp_url,
                "hostname": endpoints.hostname,
            }
        )
        # 60s TTL — refreshed on every `lifespan` startup. If the
        # service crashes the entry self-evicts within the minute.
        await client.set("crabcc:services:hitl", payload, ex=60)
        await client.aclose()
        logger.info("discovery: published to redis", extra={"redis": redis_url})
        return True
    except Exception as e:
        logger.warning("discovery redis publish failed", extra={"err": str(e)})
        return False
