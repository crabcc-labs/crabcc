"""OpenTelemetry bootstrap.

Connects to the workspace's existing ``rotel`` OTLP collector via the
standard ``OTEL_EXPORTER_OTLP_ENDPOINT`` env var (compose populates it
to ``http://rotel:4317`` — see
``crates/crabcc-core/src/service_discovery.rs:113-122``). Tracing is a
no-op when the env var is unset, so local dev outside compose stays
quiet.

We instrument:

- **FastAPI** — every incoming request becomes a span with method,
  route, status code.
- **httpx** — every outgoing call to LiteLLM (or the future MCP-HTTP
  service) becomes a child span. Cross-process trace context is
  propagated via the W3C ``traceparent`` header so the LiteLLM-side
  spans (when LiteLLM enables OTel) stitch into the same trace.

Tighter spans (per Agent step / per tool call) land in Phase 1 when
the Agents SDK exposes its own hooks.
"""

from __future__ import annotations

import logging
import os
from typing import TYPE_CHECKING

from opentelemetry import trace
from opentelemetry.exporter.otlp.proto.grpc.trace_exporter import OTLPSpanExporter
from opentelemetry.instrumentation.fastapi import FastAPIInstrumentor
from opentelemetry.instrumentation.httpx import HTTPXClientInstrumentor
from opentelemetry.sdk.resources import Resource
from opentelemetry.sdk.trace import TracerProvider
from opentelemetry.sdk.trace.export import BatchSpanProcessor

if TYPE_CHECKING:
    from fastapi import FastAPI

logger = logging.getLogger(__name__)

# Track init state — calling instrumentors twice raises in newer
# `opentelemetry-instrumentation-*` releases.
_INITIALIZED = False


def _otlp_endpoint() -> str | None:
    """Return the configured OTLP endpoint or None if telemetry is off.

    Honors both the gRPC- and HTTP-shaped env vars; gRPC wins when both
    are set because we ship the gRPC exporter (lower latency, smaller
    on-the-wire payload).
    """
    return os.environ.get("OTEL_EXPORTER_OTLP_ENDPOINT") or os.environ.get(
        "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT"
    )


def init_telemetry(*, service_name: str, service_version: str) -> bool:
    """Set up the OTel SDK + auto-instrumentation. Returns ``True`` when active.

    Idempotent: subsequent calls are no-ops. Reads the OTLP endpoint
    from the environment so deploy config + local dev share one
    convention.
    """
    global _INITIALIZED
    if _INITIALIZED:
        return True

    endpoint = _otlp_endpoint()
    if not endpoint:
        logger.info("otel disabled — OTEL_EXPORTER_OTLP_ENDPOINT unset")
        return False

    resource = Resource.create(
        {
            "service.name": service_name,
            "service.version": service_version,
            # Optional service.namespace lines services under one
            # logical app in the trace UI. Matches the Rust side's
            # convention of grouping under "crabcc".
            "service.namespace": os.environ.get("OTEL_SERVICE_NAMESPACE", "crabcc"),
            # `deployment.environment` lets the trace UI separate
            # local-compose runs from staging / prod.
            "deployment.environment": os.environ.get("OTEL_DEPLOYMENT_ENVIRONMENT", "dev"),
        }
    )
    provider = TracerProvider(resource=resource)
    # `BatchSpanProcessor` queues spans and flushes them to the
    # collector on a timer; far cheaper than a per-span gRPC call.
    # Defaults: 5s flush, 512-span queue, 30s timeout — sane.
    provider.add_span_processor(BatchSpanProcessor(OTLPSpanExporter(endpoint=endpoint)))
    trace.set_tracer_provider(provider)

    # httpx instrumentation works at the AsyncClient/Client class level;
    # any client we build after this call gets traced. Done early in
    # lifespan so the openai SDK's underlying client is covered.
    HTTPXClientInstrumentor().instrument()

    logger.info("otel ready", extra={"endpoint": endpoint, "service": service_name})
    _INITIALIZED = True
    return True


def instrument_fastapi(app: FastAPI) -> None:
    """Attach FastAPI middleware that emits a span per request.

    Safe to call when telemetry is off — the instrumentor checks the
    global tracer provider and short-circuits if it's the no-op one.
    """
    FastAPIInstrumentor.instrument_app(app)
