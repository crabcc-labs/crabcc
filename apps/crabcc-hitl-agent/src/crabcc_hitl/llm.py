"""Thin wrapper around the OpenAI Agents SDK pointed at our LiteLLM proxy.

Phase 0 surface is the bare minimum: one ``Agent`` with no tools, one
``Runner.run`` per HTTP request. Phase 1 will add the crabcc MCP-HTTP
tool registry; the public ``chat`` API stays stable so the FastAPI
layer doesn't change shape.
"""

from __future__ import annotations

import logging
from typing import TYPE_CHECKING, Literal

import httpx
from agents import Agent, OpenAIChatCompletionsModel, Runner, function_tool
from openai import AsyncOpenAI

from .approvals import current_chat_id
from .tool_gate import gated
from .tools import (
    crabcc_callers,
    crabcc_files,
    crabcc_fuzzy,
    crabcc_outline,
    crabcc_refs,
    crabcc_sym,
    fetch_url,
    memory_list,
    memory_remember,
    memory_search,
)

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
        # Tool registration — Phase 2.
        # `function_tool` derives the JSON schema from each function's
        # signature + docstring. Tools handle their own "MCP not
        # configured" branch (return ``ok=False`` instead of raising)
        # so a missing crabcc-mcp service doesn't crash the loop.
        # `gated(...)` wraps each fn with the approval flow: tools whose
        # name is in ``approval_required_tools`` ask the user via
        # Telegram and block on the response; everything else runs
        # straight through.
        required = set(settings.approval_required_tools)

        def risk_for(fn_name: str) -> Literal["auto", "required"]:
            return "required" if fn_name in required else "auto"

        raw_tools = [
            fetch_url,
            crabcc_sym,
            crabcc_refs,
            crabcc_callers,
            crabcc_files,
            crabcc_outline,
            crabcc_fuzzy,
            memory_search,
            memory_remember,
            memory_list,
        ]
        tools = [function_tool(gated(fn, risk=risk_for(fn.__name__))) for fn in raw_tools]  # type: ignore[arg-type]
        self._agent = Agent(
            name="crabcc-helper",
            instructions=settings.system_prompt,
            model=self._model,
            # mypy: `list[FunctionTool]` doesn't satisfy the SDK's
            # invariant `list[FunctionTool | FileSearchTool | ...]`.
            # Runtime behaviour is correct — the SDK iterates the
            # list as `Sequence[Tool]`.
            tools=tools,  # type: ignore[arg-type]
        )
        self._max_task_chars = settings.max_task_chars

    async def chat(self, user_message: str, *, tg_chat_id: int | None = None) -> str:
        """Single round-trip: user prompt → agent reply.

        ``tg_chat_id``, when set, threads through to the approval gate
        via :data:`current_chat_id` (a :class:`contextvars.ContextVar`)
        so each tool call asks the right user. Falls back to the
        env-pinned ``telegram_owner_chat_id`` when ``None``. Long
        inputs are clipped at ``max_task_chars`` to keep prompt cost
        bounded; the user gets a short note appended.
        """
        if len(user_message) > self._max_task_chars:
            logger.warning(
                "user_message clipped",
                extra={"orig_len": len(user_message), "cap": self._max_task_chars},
            )
            user_message = (
                user_message[: self._max_task_chars] + "\n\n[message truncated by HITL agent]"
            )
        token = current_chat_id.set(tg_chat_id)
        try:
            result = await Runner.run(self._agent, user_message)
        finally:
            current_chat_id.reset(token)
        # `final_output` is the last assistant message text. The SDK
        # returns ``None`` only on tool-only flows; for the prompt-only
        # round-trip it's always a string.
        return result.final_output or ""

    async def aclose(self) -> None:
        """Release the underlying httpx pool on shutdown."""
        await self._client.close()
