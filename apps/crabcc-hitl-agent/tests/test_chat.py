"""Round-trip + auth tests for POST /chat.

The OpenAI Agents SDK is patched at the ``Runner`` boundary so the
test never reaches LiteLLM. Verifying the FastAPI wiring + auth gate
is enough for Phase 0 — the network path is exercised by the
``docker compose up`` smoke described in the README.
"""

from __future__ import annotations

import os
from collections.abc import Iterator
from types import SimpleNamespace
from typing import Any
from unittest.mock import AsyncMock, patch

import pytest
from fastapi.testclient import TestClient


@pytest.fixture(autouse=True)
def _reset_settings_singleton(monkeypatch: pytest.MonkeyPatch) -> Iterator[None]:
    """Settings is a module-level singleton — make tests independent.

    Each test sets its own env then forces ``crabcc_hitl.settings`` to
    re-import so ``Settings()`` re-reads the env. Cleaner than passing
    overrides through the app factory for a Phase-0 surface.
    Also short-circuits the startup probes + the MCP background task
    so tests don't try to reach a real LiteLLM / open MCP listener.
    """
    monkeypatch.setenv("CRABCC_HITL_LITELLM_BASE_URL", "http://litellm.test:4000")
    monkeypatch.setenv("CRABCC_HITL_LITELLM_API_KEY", "test-key")
    monkeypatch.setenv("CRABCC_HITL_MODEL", "claude-haiku-4-5-test")
    # api_token is set per-test where relevant.
    monkeypatch.delenv("CRABCC_HITL_API_TOKEN", raising=False)
    # Tests don't have an upstream LiteLLM — skip the startup gate.
    monkeypatch.setenv("CRABCC_HITL_PROBE_STARTUP_ENABLED", "false")
    # Disable the MCP sibling listener — its 9101 port would clash if
    # multiple tests start the lifespan in the same process.
    monkeypatch.setenv("CRABCC_HITL_MCP_ENABLED", "false")
    import importlib

    import crabcc_hitl.main as main_mod
    import crabcc_hitl.settings as settings_mod

    importlib.reload(settings_mod)
    importlib.reload(main_mod)
    yield


def _client_with_mocked_runner(reply: str = "hello back") -> tuple[TestClient, AsyncMock]:
    """Build a TestClient with ``Runner.run`` stubbed to a fixed reply.

    Returning the mock so tests can assert on ``called_with`` shape.
    """
    import crabcc_hitl.main as main_mod

    runner_mock = AsyncMock(return_value=SimpleNamespace(final_output=reply))
    # Patch where it's looked up, not where it's defined.
    patcher = patch("crabcc_hitl.llm.Runner.run", runner_mock)
    patcher.start()
    client = TestClient(main_mod.app)
    # The real lifespan would build an HitlAgent at startup; that touches
    # the openai client constructor. TestClient triggers lifespan on
    # __enter__ — using a context manager keeps that flow honest.
    return client, runner_mock


# ───── Tests ─────


def test_healthz_unauthenticated() -> None:
    """``/healthz`` must never require auth (k8s probes / compose deps).

    The live probes hit the (fake) LiteLLM URL set by the fixture, so
    the overall status will be ``"fail"`` — that's expected here, the
    test only verifies the endpoint is reachable, returns 200, and
    emits the documented JSON shape (status / version / checks list).
    """
    import crabcc_hitl.main as main_mod

    with TestClient(main_mod.app) as client:
        r = client.get("/healthz")
        assert r.status_code == 200
        body = r.json()
        assert body["status"] in {"ok", "degraded", "fail"}
        assert "version" in body
        assert isinstance(body["checks"], list)
        # Probe wiring: each check must surface name + status fields.
        names = {c["name"] for c in body["checks"]}
        assert "litellm" in names


def test_chat_round_trip_returns_reply() -> None:
    client, runner_mock = _client_with_mocked_runner("hi from haiku")
    with client:
        r = client.post("/chat", json={"task": "ping?"})
        assert r.status_code == 200, r.text
        body = r.json()
        assert body["reply"] == "hi from haiku"
        assert body["model"] == "claude-haiku-4-5-test"
    runner_mock.assert_awaited_once()
    # Second positional arg is the user message. ``await_args`` is
    # typed Optional by AsyncMock; assert before unpacking so mypy
    # narrows.
    assert runner_mock.await_args is not None
    args, _kwargs = runner_mock.await_args
    assert args[1] == "ping?"


def test_chat_rejects_empty_task() -> None:
    client, _ = _client_with_mocked_runner()
    with client:
        r = client.post("/chat", json={"task": ""})
        # FastAPI returns 422 for body-validation failures.
        assert r.status_code == 422


def test_chat_clips_overlong_task(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("CRABCC_HITL_MAX_TASK_CHARS", "10")
    # Force settings reload after monkeypatch.
    import importlib

    import crabcc_hitl.main as main_mod
    import crabcc_hitl.settings as settings_mod

    importlib.reload(settings_mod)
    importlib.reload(main_mod)
    client, runner_mock = _client_with_mocked_runner("clipped reply")
    with client:
        r = client.post("/chat", json={"task": "x" * 1_000})
        assert r.status_code == 200
    assert runner_mock.await_args is not None
    args, _ = runner_mock.await_args
    forwarded = args[1]
    # 10-char cap + the truncation suffix.
    assert forwarded.startswith("xxxxxxxxxx")
    assert "[message truncated by HITL agent]" in forwarded


def test_chat_requires_bearer_when_token_set(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("CRABCC_HITL_API_TOKEN", "secret-shared-with-bot")
    import importlib

    import crabcc_hitl.main as main_mod
    import crabcc_hitl.settings as settings_mod

    importlib.reload(settings_mod)
    importlib.reload(main_mod)
    client, _ = _client_with_mocked_runner()
    with client:
        # No Authorization header → 401.
        r = client.post("/chat", json={"task": "hi"})
        assert r.status_code == 401
        # Wrong scheme → 401.
        r = client.post(
            "/chat",
            json={"task": "hi"},
            headers={"Authorization": "Basic dXNlcjpwYXNz"},
        )
        assert r.status_code == 401
        # Wrong token → 401.
        r = client.post(
            "/chat",
            json={"task": "hi"},
            headers={"Authorization": "Bearer wrong"},
        )
        assert r.status_code == 401
        # Right token → 200.
        r = client.post(
            "/chat",
            json={"task": "hi"},
            headers={"Authorization": "Bearer secret-shared-with-bot"},
        )
        assert r.status_code == 200


def test_settings_module_reads_env_prefix() -> None:
    """Sanity: every Settings field comes from CRABCC_HITL_* env."""
    from crabcc_hitl.settings import Settings

    s = Settings()
    assert s.litellm_base_url == "http://litellm.test:4000"
    assert s.model == "claude-haiku-4-5-test"
    # Defaults should still apply for un-set fields.
    assert s.port == 9100


def _ensure_src_on_path() -> None:
    """Pytest invokes from repo root; src layout needs sys.path help.

    Editable install (`pip install -e .[dev]`) is the production path —
    this fallback makes ``pytest`` work without the install for quick
    iteration.
    """
    import pathlib
    import sys

    here = pathlib.Path(__file__).resolve().parent.parent
    src = here / "src"
    if str(src) not in sys.path:
        sys.path.insert(0, str(src))


_ensure_src_on_path()


# Reach into the test module's defaults so a missing env doesn't make
# the base test order-dependent.
os.environ.setdefault("CRABCC_HITL_LITELLM_BASE_URL", "http://litellm.test:4000")
os.environ.setdefault("CRABCC_HITL_LITELLM_API_KEY", "test-key")
os.environ.setdefault("CRABCC_HITL_MODEL", "claude-haiku-4-5-test")


def _silence_unused_import_warning() -> Any:
    """`Iterator` import keeps mypy happy on `_reset_settings_singleton`'s return type."""
    return Iterator
