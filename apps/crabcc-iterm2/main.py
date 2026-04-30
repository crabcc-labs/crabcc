"""crabcc iTerm2 daemon — issue #132.

AutoLaunch daemon wired into iTerm2's Python API. Provides:
  A. Status-bar HUD  (agent depth · token savings · doctor health)
  B. Custom control sequences  (crabcc:sym=<name>, crabcc:reindex)
  C. RPCs  (doctor, dashboard, search, kill_agent, remember)

Install:
  crabcc install-iterm2

Activate:
  iTerm2 → Preferences → General → Magic → Allow full Python API
  Automation permission: System Settings → Privacy & Security → Automation → iTerm2
  Then restart iTerm2 — daemon auto-launches from Scripts/AutoLaunch/.

Verify:
  crabcc doctor iterm2
"""

from __future__ import annotations

import asyncio
import json
import subprocess
import time
from typing import Any

import iterm2  # type: ignore[import-untyped]

# ── constants ─────────────────────────────────────────────────────────────────
CRABCC = "crabcc"
SERVE_BASE = "http://localhost:8090"
AGENTS_STREAM = f"{SERVE_BASE}/api/agents/stream"
AGENTS_URL = f"{SERVE_BASE}/api/agents"
DOCTOR_URL = f"{SERVE_BASE}/api/doctor"
HUD_REFRESH_S = 2
TOKEN_REFRESH_S = 60
DOCTOR_REFRESH_S = 30

# ── helpers ───────────────────────────────────────────────────────────────────

def _run(*args: str, timeout: int = 10) -> dict[str, Any]:
    """Run crabcc subcommand, return parsed JSON or empty dict on error."""
    try:
        r = subprocess.run(
            [CRABCC, *args, "--json"],
            capture_output=True, text=True, timeout=timeout,
        )
        return json.loads(r.stdout) if r.returncode == 0 else {}
    except Exception:
        return {}


def _run_text(*args: str, timeout: int = 10) -> str:
    try:
        r = subprocess.run([CRABCC, *args], capture_output=True, text=True, timeout=timeout)
        return r.stdout.strip()
    except Exception:
        return ""


async def _notify(connection: iterm2.Connection, title: str, subtitle: str = "") -> None:
    await iterm2.Alert(title=title, subtitle=subtitle).async_run(connection)


# ── status-bar state (shared across callbacks) ────────────────────────────────

class _HudState:
    agent_text: str = "🦀 idle"
    token_text: str = "—"
    doctor_glyph: str = "●"  # green = ok, orange = warn, red = fail
    _last_token: float = 0.0
    _last_doctor: float = 0.0

    def refresh_agents(self) -> None:
        try:
            import urllib.request
            with urllib.request.urlopen(AGENTS_URL, timeout=2) as resp:
                data = json.loads(resp.read())
            active = [a for a in data.get("agents", []) if a.get("status") == "running"]
            if not active:
                self.agent_text = "🦀 idle"
                return
            a = active[0]
            elapsed = int(time.time() - a.get("started_ts", time.time()))
            m, s = divmod(elapsed, 60)
            name = a.get("name", "agent")[:20]
            depth = len(active)
            self.agent_text = f"🦀 {name} · {m}m{s:02d}s" + (f" (+{depth-1})" if depth > 1 else "")
        except Exception:
            self.agent_text = "🦀 —"

    def refresh_tokens(self) -> None:
        now = time.time()
        if now - self._last_token < TOKEN_REFRESH_S:
            return
        self._last_token = now
        data = _run("track")
        saved = data.get("saved_tokens_today", 0)
        if saved >= 1_000_000:
            self.token_text = f"saved {saved/1e6:.1f}M tok"
        elif saved >= 1_000:
            self.token_text = f"saved {saved//1000}k tok"
        else:
            self.token_text = "—"

    def refresh_doctor(self) -> None:
        now = time.time()
        if now - self._last_doctor < DOCTOR_REFRESH_S:
            return
        self._last_doctor = now
        data = _run("doctor")
        checks = data.get("checks", [])
        if any(c.get("status") == "fail" for c in checks):
            self.doctor_glyph = "🔴"
        elif any(c.get("status") == "warn" for c in checks):
            self.doctor_glyph = "🟡"
        else:
            self.doctor_glyph = "🟢"


_state = _HudState()


# ── main entry ────────────────────────────────────────────────────────────────

async def main(connection: iterm2.Connection) -> None:
    app = await iterm2.async_get_app(connection)

    # ── A. Status-bar component ───────────────────────────────────────────────
    component = iterm2.StatusBarComponent(
        short_description="crabcc HUD",
        detailed_description="Active agent · token savings · doctor health",
        knobs=[],
        exemplar="🦀 warp-speed-audit · 4m12s  |  saved 1.2M tok  |  🟢",
        update_cadence=HUD_REFRESH_S,
        identifier="com.crabcc.hud",
    )

    @iterm2.StatusBarRPC
    async def crabcc_hud(_knobs):
        _state.refresh_agents()
        _state.refresh_tokens()
        _state.refresh_doctor()
        return (
            f"{_state.agent_text}"
            f"  |  {_state.token_text}"
            f"  |  {_state.doctor_glyph}"
        )

    await component.async_register(connection, crabcc_hud)

    # ── C. RPCs ───────────────────────────────────────────────────────────────

    @iterm2.RPC
    async def crabcc_doctor():
        """Run all doctor checks; alert on first failure."""
        data = _run("doctor")
        first_fail = next(
            (c for c in data.get("checks", []) if c.get("status") != "ok"), None
        )
        if first_fail:
            await _notify(
                connection,
                title=f"crabcc doctor: {first_fail['check']} — {first_fail['status']}",
                subtitle=first_fail.get("hint", ""),
            )
        else:
            await _notify(connection, title="crabcc doctor: all checks passed ✓")

    await crabcc_doctor.async_register(connection)

    @iterm2.RPC
    async def crabcc_dashboard():
        """Open the /live dashboard in a new browser tab."""
        subprocess.Popen(["open", f"{SERVE_BASE}/live"])

    await crabcc_dashboard.async_register(connection)

    @iterm2.RPC
    async def crabcc_search(q=iterm2.Reference("?query")):
        """Search crabcc memory; show results in a scratch tab."""
        if not q:
            return
        result = _run_text("memory", "search", q, "--limit", "5")
        window = app.current_terminal_window
        if window:
            tab = await window.async_create_tab()
            session = tab.current_session
            await session.async_send_text(
                f"echo {json.dumps(result)} | jq . | less\n"
            )

    await crabcc_search.async_register(connection)

    @iterm2.RPC
    async def crabcc_kill_agent(agent_id=iterm2.Reference("?agent_id")):
        """Kill a running agent by ID with confirmation."""
        if not agent_id:
            return
        confirmed = await iterm2.Alert(
            title=f"Kill agent {agent_id}?",
            subtitle="This sends SIGTERM to the agent process.",
            buttons=["Kill", "Cancel"],
        ).async_run(connection)
        if confirmed == 0:
            _run_text("agent-kill", agent_id)

    await crabcc_kill_agent.async_register(connection)

    @iterm2.RPC
    async def crabcc_remember(body=iterm2.Reference("session.contents")):
        """Save current session contents as a memory drawer."""
        if not body:
            return
        session = app.current_terminal_window.current_tab.current_session
        sid = session.session_id
        _run_text("memory", "remember", f"session:{sid}", body[:4000])
        await _notify(connection, title="crabcc memory: session saved ✓")

    await crabcc_remember.async_register(connection)

    # ── B. Custom control sequences ───────────────────────────────────────────

    # crabcc:sym=<name>  — look up a symbol definition
    async with iterm2.CustomControlSequenceMonitor(
        connection, "crabcc", r"^sym=(?P<name>.+)$"
    ) as sym_mon:
        async def _handle_sym():
            while True:
                match = await sym_mon.async_get()
                name = match.group("name")
                result = _run_text("sym", name)
                window = app.current_terminal_window
                if window:
                    tab = await window.async_create_tab()
                    await tab.current_session.async_send_text(
                        f"echo {json.dumps(result)} | jq . | less\n"
                    )

        asyncio.ensure_future(_handle_sym())

    # crabcc:reindex  — trigger a re-index when stale post-merge
    async with iterm2.CustomControlSequenceMonitor(
        connection, "crabcc", r"^reindex$"
    ) as reindex_mon:
        async def _handle_reindex():
            while True:
                await reindex_mon.async_get()
                window = app.current_terminal_window
                if window:
                    tab = await window.async_create_tab()
                    await tab.current_session.async_send_text(
                        "crabcc index && echo '✓ crabcc index complete'\n"
                    )

        asyncio.ensure_future(_handle_reindex())

    # Keep the daemon alive
    await asyncio.sleep(float("inf"))


iterm2.run_forever(main)
