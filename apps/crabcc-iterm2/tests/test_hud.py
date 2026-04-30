"""Unit tests for the crabcc iTerm2 HUD daemon — issue #132.

Tests the pure-Python logic (state refresh, text formatting) without requiring
a live iTerm2 connection. The async iTerm2 surfaces are stubbed.
"""

from __future__ import annotations

import json
import sys
import time
import unittest
from pathlib import Path
from unittest.mock import MagicMock, patch

# Add parent to path so we can import main without installing the package.
sys.path.insert(0, str(Path(__file__).parent.parent))

# Stub the iterm2 module so the file imports cleanly outside iTerm2.
iterm2_stub = MagicMock()
sys.modules.setdefault("iterm2", iterm2_stub)

import main as m  # noqa: E402  (after stub)


class TestHudStateAgents(unittest.TestCase):
    def _make_state(self) -> m._HudState:
        return m._HudState()

    def test_idle_when_no_agents(self) -> None:
        state = self._make_state()
        with patch("urllib.request.urlopen") as mock_open:
            mock_open.return_value.__enter__.return_value.read.return_value = json.dumps(
                {"agents": []}
            ).encode()
            state.refresh_agents()
        self.assertEqual(state.agent_text, "🦀 idle")

    def test_running_agent_shows_name_and_elapsed(self) -> None:
        state = self._make_state()
        started = time.time() - 252  # 4m12s ago
        payload = json.dumps({
            "agents": [{"status": "running", "name": "warp-speed-audit", "started_ts": started}]
        }).encode()
        with patch("urllib.request.urlopen") as mock_open:
            mock_open.return_value.__enter__.return_value.read.return_value = payload
            state.refresh_agents()
        self.assertIn("warp-speed-audit", state.agent_text)
        self.assertIn("4m", state.agent_text)

    def test_multiple_agents_shows_count(self) -> None:
        state = self._make_state()
        agents = [
            {"status": "running", "name": "agent-a", "started_ts": time.time() - 10},
            {"status": "running", "name": "agent-b", "started_ts": time.time() - 5},
        ]
        payload = json.dumps({"agents": agents}).encode()
        with patch("urllib.request.urlopen") as mock_open:
            mock_open.return_value.__enter__.return_value.read.return_value = payload
            state.refresh_agents()
        self.assertIn("+1", state.agent_text)

    def test_network_error_shows_dash(self) -> None:
        state = self._make_state()
        with patch("urllib.request.urlopen", side_effect=OSError("refused")):
            state.refresh_agents()
        self.assertEqual(state.agent_text, "🦀 —")


class TestHudStateTokens(unittest.TestCase):
    def test_million_tokens_formats_with_M(self) -> None:
        state = m._HudState()
        state._last_token = 0  # force refresh
        with patch.object(m, "_run", return_value={"saved_tokens_today": 1_234_567}):
            state.refresh_tokens()
        self.assertIn("1.2M", state.token_text)

    def test_thousands_formats_with_k(self) -> None:
        state = m._HudState()
        state._last_token = 0
        with patch.object(m, "_run", return_value={"saved_tokens_today": 42_000}):
            state.refresh_tokens()
        self.assertIn("42k", state.token_text)

    def test_throttles_at_60s(self) -> None:
        state = m._HudState()
        state._last_token = time.time()  # just refreshed
        call_count = [0]
        original = m._run

        def counting_run(*a, **kw):
            call_count[0] += 1
            return original(*a, **kw)

        with patch.object(m, "_run", side_effect=counting_run):
            state.refresh_tokens()
        self.assertEqual(call_count[0], 0, "should skip refresh within 60s window")


class TestHudStateDoctor(unittest.TestCase):
    def test_all_ok_shows_green(self) -> None:
        state = m._HudState()
        state._last_doctor = 0
        checks = [{"check": "index", "status": "ok"}, {"check": "graph", "status": "ok"}]
        with patch.object(m, "_run", return_value={"checks": checks}):
            state.refresh_doctor()
        self.assertEqual(state.doctor_glyph, "🟢")

    def test_warn_shows_yellow(self) -> None:
        state = m._HudState()
        state._last_doctor = 0
        checks = [{"check": "index", "status": "warn"}, {"check": "graph", "status": "ok"}]
        with patch.object(m, "_run", return_value={"checks": checks}):
            state.refresh_doctor()
        self.assertEqual(state.doctor_glyph, "🟡")

    def test_fail_shows_red(self) -> None:
        state = m._HudState()
        state._last_doctor = 0
        checks = [{"check": "index", "status": "fail"}]
        with patch.object(m, "_run", return_value={"checks": checks}):
            state.refresh_doctor()
        self.assertEqual(state.doctor_glyph, "🔴")

    def test_api_error_keeps_previous_glyph(self) -> None:
        state = m._HudState()
        state._last_doctor = 0
        state.doctor_glyph = "🟢"
        with patch.object(m, "_run", return_value={}):
            state.refresh_doctor()
        # empty checks list → no fail/warn → stays green
        self.assertEqual(state.doctor_glyph, "🟢")


class TestRunHelper(unittest.TestCase):
    def test_returns_parsed_json_on_success(self) -> None:
        with patch("subprocess.run") as mock_run:
            mock_run.return_value = MagicMock(returncode=0, stdout='{"ok":true}')
            result = m._run("doctor")
        self.assertEqual(result, {"ok": True})

    def test_returns_empty_dict_on_nonzero_exit(self) -> None:
        with patch("subprocess.run") as mock_run:
            mock_run.return_value = MagicMock(returncode=1, stdout="")
            result = m._run("doctor")
        self.assertEqual(result, {})

    def test_returns_empty_dict_on_exception(self) -> None:
        with patch("subprocess.run", side_effect=FileNotFoundError("crabcc not found")):
            result = m._run("doctor")
        self.assertEqual(result, {})


if __name__ == "__main__":
    unittest.main()
