"""Thin wrapper around the OpenAI Agents SDK pointed at our LiteLLM proxy.

Phase 0 surface is the bare minimum: one ``Agent`` with no tools, one
``Runner.run`` per HTTP request. Phase 1 will add the crabcc MCP-HTTP
tool registry; the public ``chat`` API stays stable so the FastAPI
layer doesn't change shape.
"""

from __future__ import annotations

import logging
from typing import TYPE_CHECKING

import httpx
from agents import Agent, OpenAIChatCompletionsModel, Runner
from openai import AsyncOpenAI

if TYPE_CHECKING:
    from .settings import Settings

logger = logging.getLogger(__name__)


def build_httpx_client(settings: Settings) -> httpx.AsyncClient:
    """Construct the long-lived httpx pool used by both the agent and probes.

    Pre-tuned for a single-upstream service: HTTP/2 multiplexing, a
    keep-alive pool sized for the expected concurrency, and finer
    timeout granularity than openai's defaults. Sharing the client
    across components means probes warm the pool that the agent then
    reuses — first agent call after startup hits a hot connection.
    """
    return httpx.AsyncClient(
        http2=settings.httpx_http2,
        timeout=httpx.Timeout(
            connect=settings.httpx_connect_timeout_s,
            read=settings.httpx_read_timeout_s,
            write=settings.httpx_write_timeout_s,
            pool=settings.httpx_connect_timeout_s,
        ),
        limits=httpx.Limits(
            max_connections=settings.httpx_max_connections,
            max_keepalive_connections=settings.httpx_max_keepalive_connections,
            keepalive_expiry=settings.httpx_keepalive_expiry_s,
        ),
        # Trust environment proxy vars (HTTPS_PROXY etc.) but don't
        # follow redirects automatically — LiteLLM never redirects;
        # blindly following would mask config errors.
        follow_redirects=False,
    )


class HitlAgent:
    """Owns the long-lived OpenAI client + Agent definition."""

    def __init__(self, settings: Settings, http_client: httpx.AsyncClient) -> None:
        # Reuse the shared httpx pool so the openai SDK + the probe
        # path share connections. `max_retries=0` because retries are
        # LiteLLM's job (it has its own fallback chain configured in
        # install/ollama-stack/litellm.config.yaml).
        self._http_client = http_client
        self._client = AsyncOpenAI(
            base_url=settings.litellm_base_url,
            api_key=settings.litellm_api_key,
            max_retries=0,
            http_client=http_client,
        )
        # The Agents SDK's default model class targets the OpenAI
        # Responses API; LiteLLM only speaks the older Chat Completions
        # shape, so we wrap explicitly.
        self._model = OpenAIChatCompletionsModel(
            model=settings.model,
            openai_client=self._client,
        )
        self._agent = Agent(
            name="crabcc-helper",
            instructions=settings.system_prompt,
            model=self._model,
        )
        self._max_task_chars = settings.max_task_chars

    async def chat(self, user_message: str) -> str:
        """Single round-trip: user prompt → agent reply.

        No tool calls, no session state — Phase 0. Returns the model's
        final text. Long inputs are clipped at ``max_task_chars`` to
        keep prompt cost bounded; the user gets a short note appended.
        """
        if len(user_message) > self._max_task_chars:
            logger.warning(
                "user_message clipped",
                extra={"orig_len": len(user_message), "cap": self._max_task_chars},
            )
            user_message = (
                user_message[: self._max_task_chars] + "\n\n[message truncated by HITL agent]"
            )
        result = await Runner.run(self._agent, user_message)
        # `final_output` is the last assistant message text. The SDK
        # returns ``None`` only on tool-only flows (Phase 1+); for
        # Phase 0 it's always a string.
        return result.final_output or ""

    async def aclose(self) -> None:
        """Release the underlying httpx pool on shutdown."""
        await self._client.close()
