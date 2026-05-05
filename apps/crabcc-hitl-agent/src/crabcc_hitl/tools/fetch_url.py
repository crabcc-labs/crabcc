"""Fetch a URL and convert its content to clean markdown.

Uses Microsoft's `markitdown` library — broader surface than HTML-only
parsers (also handles PDFs, DOCX, XLSX, PPTX) and keeps boilerplate
out of the agent's context window.

The HTTP fetch itself goes through `httpx.AsyncClient` (the same h2
pool the rest of the service uses; tracing covers it for free).
SSRF guard mirrors the Rust ``crabcc-fetch::is_ingest_safe_url``
defaults so this tool can't be used to probe internal services from
a chat prompt.
"""

from __future__ import annotations

import asyncio
import io
import logging
from typing import Final

import httpx
from pydantic import BaseModel, Field

logger = logging.getLogger(__name__)

# Mirrors the cap baked into ``crabcc-fetch::INGEST_MAX_BODY_BYTES``
# (5 MiB). Chats don't need bigger context than that; bigger blobs
# are usually a sign of a misclick (uploaded video, etc.).
_MAX_BYTES: Final[int] = 5 * 1024 * 1024
_DEFAULT_TIMEOUT_S: Final[float] = 15.0
_USER_AGENT: Final[str] = "crabcc-hitl-agent/0.1.0 (+https://github.com/peterlodri-sec/crabcc)"


class FetchResult(BaseModel):
    """Structured outcome — agents can branch on `ok` instead of catching exceptions."""

    ok: bool
    url: str
    title: str | None = Field(default=None, description="Document title when known.")
    markdown: str | None = Field(
        default=None, description="Cleaned markdown body. None on failure."
    )
    bytes_read: int = 0
    error: str | None = None


def _is_safe_url(url: str) -> tuple[bool, str | None]:
    """Reject loopback / RFC1918 / link-local / non-http(s) URLs.

    Same shape as ``crates/crabcc-fetch/src/lib.rs::is_ingest_safe_url``
    so policy stays consistent across the Rust + Python bot surfaces.
    """
    if "://" not in url:
        return False, "missing scheme"
    scheme, rest = url.split("://", 1)
    if scheme not in {"http", "https"}:
        return False, f"scheme `{scheme}` not allowed"
    authority = rest.split("/", 1)[0].split("?", 1)[0]
    if "@" in authority:
        authority = authority.rsplit("@", 1)[1]
    # Strip IPv6 brackets / port suffix.
    host = authority
    if host.startswith("["):
        end = host.find("]")
        host = host[1:end] if end > 0 else host
    elif ":" in host:
        host = host.rsplit(":", 1)[0]
    host_lower = host.lower()
    blocked_exact = {
        "localhost",
        "0.0.0.0",
        "broadcasthost",
        "ip6-localhost",
        "ip6-loopback",
        "::1",
        "::",
    }
    if host_lower in blocked_exact or host_lower.endswith(".localhost"):
        return False, "loopback host not allowed"
    # IPv4 RFC1918 / link-local / loopback.
    try:
        import ipaddress

        addr = ipaddress.ip_address(host_lower)
        if addr.is_loopback or addr.is_private or addr.is_link_local or addr.is_unspecified:
            return False, f"private/internal IP `{addr}` not allowed"
    except ValueError:
        pass  # Not a literal — fine.
    return True, None


async def fetch_url(url: str, *, timeout_s: float = _DEFAULT_TIMEOUT_S) -> FetchResult:
    """Download a URL and return its content as clean markdown.

    Args:
        url: HTTP or HTTPS URL.
        timeout_s: Max wall time for the download, in seconds.

    Returns:
        :class:`FetchResult`. ``ok=False`` on any error — fields
        ``error`` (human readable) and ``url`` (echo) are always set.

    Side effects: none. The fetch is read-only; markdown conversion
    runs in a worker thread (markitdown's pdf path is CPU-bound and
    would block the event loop otherwise).
    """
    logger.debug("fetch_url: incoming", extra={"url": url})
    safe, why = _is_safe_url(url)
    if not safe:
        logger.warning("fetch_url: SSRF guard tripped", extra={"url": url, "reason": why})
        return FetchResult(ok=False, url=url, error=f"refused: {why}")

    # Use a short-lived client so we don't share connection state
    # with the LLM upstream — mixing pools confuses tracing and a
    # slow target site shouldn't tie up the openai pool.
    headers = {"User-Agent": _USER_AGENT, "Accept": "*/*"}
    try:
        async with httpx.AsyncClient(
            timeout=timeout_s,
            headers=headers,
            follow_redirects=True,
            limits=httpx.Limits(max_connections=4),
        ) as client:
            resp = await client.get(url)
    except httpx.HTTPError as e:
        logger.info("fetch_url: connect failed", extra={"url": url, "err": str(e)})
        return FetchResult(ok=False, url=url, error=f"http error: {e}")

    if resp.status_code >= 400:
        return FetchResult(
            ok=False,
            url=url,
            bytes_read=len(resp.content),
            error=f"http {resp.status_code}",
        )
    if len(resp.content) > _MAX_BYTES:
        return FetchResult(
            ok=False,
            url=url,
            bytes_read=len(resp.content),
            error=f"body too big ({len(resp.content)} > {_MAX_BYTES} bytes)",
        )

    # markitdown is sync + CPU-bound on PDFs/Office formats; offload.
    # We hand it bytes + the source URL so it can pick the right
    # converter from `Content-Type` and the URL extension together.
    content_type = resp.headers.get("content-type", "")

    def _convert() -> tuple[str | None, str | None]:
        from markitdown import MarkItDown

        md = MarkItDown()
        # `convert_stream` accepts a BytesIO + url-style hint; this
        # keeps the call shape uniform across HTML / PDF / DOCX /
        # XLSX inputs without us sniffing the format ourselves.
        result = md.convert_stream(io.BytesIO(resp.content), url=url)
        return result.text_content, result.title

    try:
        markdown, title = await asyncio.to_thread(_convert)
    except Exception as e:
        logger.warning("fetch_url: convert failed", extra={"url": url, "err": str(e)})
        return FetchResult(
            ok=False, url=url, bytes_read=len(resp.content), error=f"convert error: {e}"
        )

    logger.info(
        "fetch_url: ok",
        extra={
            "url": url,
            "bytes_read": len(resp.content),
            "content_type": content_type,
            "markdown_chars": len(markdown) if markdown else 0,
        },
    )
    return FetchResult(
        ok=True,
        url=url,
        title=title,
        markdown=markdown,
        bytes_read=len(resp.content),
    )
