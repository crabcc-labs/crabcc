"""Expose the HITL agent's ``chat`` capability as an MCP tool.

Lets other host services (Rust crabcc-mcp consumers, future agents in
the workspace) call us through the same protocol crabcc itself
speaks. Mounted on a separate port (``CRABCC_HITL_MCP_PORT``, default
9101) so the FastAPI HTTP API on 9100 stays clean for the bot.

Bulletproofing:

- Background task is wrapped in a try/except so a transport-bind
  failure (port in use, IPv6 stack misconfigured) logs an error
  with context instead of silently dying.
- :func:`probe_mcp_started` runs after the task spawns and verifies
  the listener actually accepted a connection — catches ``port in
  use`` errors that the SDK can swallow.
- Every tool call logs ``info`` with task length + reply length;
  errors log with the exception class so Sentry-like collectors
  can group by error shape.
"""

from __future__ import annotations

import asyncio
import logging
import socket
from typing import TYPE_CHECKING

from mcp.server.fastmcp import FastMCP

if TYPE_CHECKING:
    from .llm import HitlAgent

logger = logging.getLogger(__name__)


def build_mcp(agent: HitlAgent, *, port: int) -> FastMCP:
    """Build an :class:`FastMCP` server exposing the HITL ``chat`` tool.

    The returned object hasn't started its transport yet; the caller
    spawns it on its own asyncio loop. Tool schema mirrors the FastAPI
    ``/chat`` body so clients can swap between REST and MCP without
    changing call shapes.
    """
    mcp = FastMCP(
        name="crabcc-hitl-agent",
        # The default `streamable-http` transport runs on 0.0.0.0:port.
        # Loopback-only mapping happens at the docker level (see
        # docker-compose.yml).
        host="0.0.0.0",
        port=port,
        streamable_http_path="/mcp",
    )

    @mcp.tool()
    async def chat(task: str) -> str:
        """Round-trip a prompt through the LiteLLM-backed agent.

        Args:
            task: The user's prompt verbatim. Same shape as POST /chat.

        Returns:
            The model's text reply.
        """
        logger.debug("mcp.chat: incoming", extra={"task_len": len(task)})
        try:
            reply = await agent.chat(task)
        except Exception as e:
            logger.error(
                "mcp.chat: failed",
                extra={"task_len": len(task), "error_class": type(e).__name__, "error": str(e)},
            )
            raise
        logger.info(
            "mcp.chat: completed",
            extra={"task_len": len(task), "reply_len": len(reply)},
        )
        return reply

    logger.info("mcp server configured", extra={"tool_count": 1, "port": port})
    return mcp


async def run_mcp(mcp: FastMCP) -> None:
    """Run the MCP server's streamable-HTTP transport.

    Spawned as a background task during the FastAPI ``lifespan``;
    cancelled on shutdown. Wraps the SDK call so a transport bind
    error (port in use, etc.) lands in the log instead of as a silent
    background-task exit.
    """
    logger.info("mcp server starting")
    try:
        await mcp.run_streamable_http_async()
    except asyncio.CancelledError:
        logger.info("mcp server stopping (cancelled)")
        raise
    except Exception as e:
        logger.error(
            "mcp server crashed",
            extra={"error_class": type(e).__name__, "error": str(e)},
        )
        raise


async def probe_mcp_started(*, host: str, port: int, timeout_s: float = 2.0) -> bool:
    """Verify the MCP transport actually bound and is accepting TCP.

    Polls the port with :mod:`socket` (sync, ~ms) for up to
    ``timeout_s``; returns True on first success. Used right after
    spawning the background task in lifespan so a "the SDK said it
    started but the OS rejected the bind" failure surfaces as a clear
    log line + non-degraded readiness reporting.
    """
    deadline_loops = max(1, int(timeout_s * 20))  # 50ms steps
    target_host = host or "127.0.0.1"
    if target_host == "0.0.0.0":
        target_host = "127.0.0.1"
    for _ in range(deadline_loops):
        try:
            with socket.create_connection((target_host, port), timeout=0.05):
                logger.info("mcp probe: listener up", extra={"port": port})
                return True
        except OSError:
            await asyncio.sleep(0.05)
    logger.error(
        "mcp probe: listener never came up",
        extra={"port": port, "timeout_s": timeout_s},
    )
    return False
