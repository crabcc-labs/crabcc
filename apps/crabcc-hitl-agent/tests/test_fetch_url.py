"""Sanity tests for the ``fetch_url`` tool.

Network is mocked at the httpx layer so the test is hermetic. The
markitdown call still runs for real on the (tiny) HTML body, which
exercises the conversion path end-to-end without needing a network.
"""

from __future__ import annotations

import pytest

from crabcc_hitl.tools.fetch_url import _is_safe_url, fetch_url

# ───── SSRF guard ─────


def test_ssrf_rejects_loopback_and_private() -> None:
    bad = [
        "http://localhost/",
        "http://127.0.0.1/admin",
        "http://10.0.0.1/",
        "http://192.168.1.1/",
        "http://169.254.169.254/",  # AWS IMDS
        "http://[::1]/",
        "http://[fe80::1]/",
        "http://0.0.0.0/",
        "ftp://example.com/",
        "file:///etc/passwd",
        "http://server.localhost/",
    ]
    for u in bad:
        ok, why = _is_safe_url(u)
        assert not ok, f"expected reject: {u}"
        assert why


def test_ssrf_allows_public() -> None:
    good = [
        "http://example.com/",
        "https://example.com/path",
        "https://1.1.1.1/",
        "https://user:pass@example.com/",
    ]
    for u in good:
        ok, _ = _is_safe_url(u)
        assert ok, f"expected accept: {u}"


# ───── Conversion + error handling ─────


@pytest.mark.asyncio
async def test_fetch_url_rejects_ssrf_target() -> None:
    res = await fetch_url("http://127.0.0.1/admin")
    assert res.ok is False
    # Either guard arm catches the loopback IP — `_is_safe_url`'s
    # ipaddress branch fires first and reports "private/internal IP";
    # the literal-string branch reports "loopback host". Accept both
    # so the test isn't coupled to the guard's internal ordering.
    err = (res.error or "").lower()
    assert any(s in err for s in ("loopback", "private", "internal"))
    assert res.markdown is None


@pytest.mark.asyncio
async def test_fetch_url_converts_html(monkeypatch: pytest.MonkeyPatch) -> None:
    """Patches httpx so we don't hit the network — markitdown runs for real."""
    import httpx

    html = b"<html><head><title>Hi</title></head><body><h1>Hello</h1><p>world</p></body></html>"

    class _FakeResp:
        status_code = 200
        content = html
        headers = {"content-type": "text/html; charset=utf-8"}

    class _FakeClient:
        def __init__(self, *a: object, **kw: object) -> None:
            pass

        async def __aenter__(self) -> _FakeClient:
            return self

        async def __aexit__(self, *a: object) -> None:
            return None

        async def get(self, _url: str) -> _FakeResp:
            return _FakeResp()

    monkeypatch.setattr(httpx, "AsyncClient", _FakeClient)

    res = await fetch_url("https://example.com/page")
    assert res.ok is True, res.error
    assert res.bytes_read == len(html)
    assert res.markdown is not None
    # markitdown's HTML path renders headings as `# `; verify the body
    # text survived without tightly coupling to the converter's
    # exact output.
    assert "Hello" in res.markdown
    assert "world" in res.markdown


@pytest.mark.asyncio
async def test_fetch_url_returns_http_error(monkeypatch: pytest.MonkeyPatch) -> None:
    import httpx

    class _FakeResp:
        status_code = 404
        content = b"not found"
        headers = {"content-type": "text/plain"}

    class _FakeClient:
        def __init__(self, *a: object, **kw: object) -> None:
            pass

        async def __aenter__(self) -> _FakeClient:
            return self

        async def __aexit__(self, *a: object) -> None:
            return None

        async def get(self, _url: str) -> _FakeResp:
            return _FakeResp()

    monkeypatch.setattr(httpx, "AsyncClient", _FakeClient)

    res = await fetch_url("https://example.com/missing")
    assert res.ok is False
    assert "http 404" in (res.error or "")
