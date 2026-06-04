#!/usr/bin/env bash
# crabcc-agent-runner entrypoint.
#
# Wraps `claude code -p "<prompt>"` (passed by the parent worker as
# the container CMD) with:
#   * RTK transparent CLI proxy (if CRABCC_RTK=1 and rtk on PATH).
#   * context-mode environment hooks (if CRABCC_CONTEXT_MODE=1).
#   * Sandbox flags driven by env (`CLAUDE_DISABLE_BASH`, etc.).
#
# tini is PID1 (set in Dockerfile ENTRYPOINT) so any orphans we
# fork-and-forget get reaped.

set -euo pipefail

# --- RTK ---------------------------------------------------------------
if [[ "${CRABCC_RTK:-0}" == "1" ]] && command -v rtk >/dev/null 2>&1; then
  echo "[crabcc-agent] rtk enabled — $(rtk --version 2>/dev/null || true)" >&2
else
  unset CRABCC_RTK
fi

# --- context-mode ------------------------------------------------------
if [[ "${CRABCC_CONTEXT_MODE:-0}" == "1" ]]; then
  if command -v context-mode >/dev/null 2>&1; then
    echo "[crabcc-agent] context-mode enabled" >&2
    # context-mode CLI typically shims via PreToolUse hook + MCP
    # registration. The agent picks this up through CLAUDE_CODE_HOOKS
    # which the host service sets per-job.
    export CLAUDE_CODE_CONTEXT_MODE=1
  else
    echo "[crabcc-agent] context-mode requested but not installed" >&2
  fi
fi

# --- axint -------------------------------------------------------------
# axint MCP server is registered via /etc/crabcc-agent/mcp.json (or
# /tmp/mcp.json when AXINT_MCP_URL is set; see above). Verify the in-
# container bin is on PATH unless we're in host-axint mode (where the
# in-container bin is unused).
if [[ -n "${AXINT_MCP_URL:-}" ]]; then
  : # in host mode — agent reaches axint-mcp-http over HTTP, no in-container bin needed.
elif command -v axint >/dev/null 2>&1; then
  echo "[crabcc-agent] axint MCP available — $(axint --version 2>/dev/null || echo 'unknown')" >&2
else
  echo "[crabcc-agent] WARN: axint missing — axint MCP tools will be unavailable" >&2
fi

# --- crabcc ------------------------------------------------------------
# crabcc gives the agent code intelligence (sym/refs/callers/outline/memory)
# via its stdio MCP server, wired in mcp.json. Best-effort index the workspace
# at startup so the first symbol queries are warm. The tmpfs workspace means
# the index is per-container (cheap for a single repo). Never fatal (Type-4):
# a failed/absent crabcc must not block the agent run. Disable with CRABCC_INDEX=0.
if command -v crabcc >/dev/null 2>&1; then
  echo "[crabcc-agent] crabcc available — $(crabcc --version 2>/dev/null || echo 'unknown')" >&2
  if [[ "${CRABCC_INDEX:-1}" == "1" && -d /workspace ]]; then
    if ( cd /workspace && crabcc index >/dev/null 2>&1 ); then
      echo "[crabcc-agent] crabcc index built for /workspace" >&2
    else
      echo "[crabcc-agent] crabcc index skipped/failed (non-fatal)" >&2
    fi
  fi
else
  echo "[crabcc-agent] WARN: crabcc missing — code-intel MCP tools will be unavailable" >&2
fi

# --- sandbox flags translated to Claude Code CLI args ----------------
CLAUDE_ARGS=()
if [[ "${CLAUDE_DISABLE_BASH:-0}" == "1" ]]; then
  CLAUDE_ARGS+=("--disallow-tool" "Bash")
fi

# Model — service config defaults to claude-sonnet-4-6; per-job env
# can override.
CLAUDE_ARGS+=("--model" "${CLAUDE_MODEL:-claude-sonnet-4-6}")

# Reasoning effort — translated to a system-prompt directive. Claude
# Code reads `--append-system-prompt` and concatenates onto its own
# system prompt. We deliberately avoid `--max-thinking-tokens` because
# the flag's CLI name has shifted between Claude Code versions; the
# system-prompt directive is stable.
case "${CLAUDE_EFFORT:-high}" in
  high)   _effort_directive="Effort: high. Take time to think carefully. Explore alternatives, verify assumptions, and double-check your work before responding." ;;
  medium) _effort_directive="Effort: medium. Think before acting; verify assumptions on critical paths." ;;
  low)    _effort_directive="Effort: low. Be concise; prefer fast direct answers over extensive exploration." ;;
  *)      _effort_directive="Effort: ${CLAUDE_EFFORT}." ;;
esac
CLAUDE_ARGS+=("--append-system-prompt" "${_effort_directive}")

# MCP wiring.
#
# Two modes:
#  - host-axint mode (AXINT_MCP_URL set): connect to the host's
#    axint-mcp-http over HTTP via `host.docker.internal`. Shares the
#    host's warm caches / project memory pack / fix-packet history.
#  - default mode: use the baked stdio config (`axint mcp` in-container).
#
# host-axint mode writes a fresh JSON to /tmp (tmpfs, writable) — the
# baked /etc config is read-only and stays untouched.
if [[ -n "${AXINT_MCP_URL:-}" ]]; then
  cat > /tmp/mcp.json <<EOF
{
  "mcpServers": {
    "axint": {
      "type": "http",
      "url": "${AXINT_MCP_URL}"
    },
    "crabcc": {
      "command": "crabcc",
      "args": ["--mcp"],
      "env": { "CRABCC_NO_TELEMETRY": "1" }
    }
  }
}
EOF
  CLAUDE_ARGS+=("--mcp-config" "/tmp/mcp.json")
  echo "[crabcc-agent] axint MCP via HTTP → ${AXINT_MCP_URL}" >&2
elif [[ -f /etc/crabcc-agent/mcp.json ]]; then
  CLAUDE_ARGS+=("--mcp-config" "/etc/crabcc-agent/mcp.json")
fi

# Always run sandboxed. See https://code.claude.com/docs/en/sandboxing.
CLAUDE_ARGS+=("--sandbox")

# Non-interactive: skip permission prompts, no TTY, no spinner. The
# parent container is launched with `tty=false` (bollard HostConfig) so
# stdin is closed; we still pin the permission mode explicitly so a
# stray prompt path can't block the run.
CLAUDE_ARGS+=("--permission-mode" "bypassPermissions")
export CLAUDE_NONINTERACTIVE=1
export CI=1

# --- Dispatch on AGENT_KIND ------------------------------------------
#
# CMD shape from the worker is `agent <prompt>` (see runner.rs). We
# branch here on AGENT_KIND env to pick the actual CLI to invoke.
# Default: claude-code.
#
# Stdin redirect from /dev/null is belt-and-braces: even if a future
# caller passes a TTY, neither agent can read from it.

if [[ "${1:-}" == "agent" ]]; then
  shift
  PROMPT_ARG="${1:-${PROMPT:-}}"

  case "${AGENT_KIND:-claude-code}" in
    mini-swe|mini)
      # mini-swe-agent (https://mini-swe-agent.com). Configuration:
      #   --config /etc/crabcc-agent/mini.yaml   baked-in defaults
      #   --yolo                                 never prompt
      #   --model "$CLAUDE_MODEL"                per-job model
      #   --step-limit / --cost-limit            optional per-job caps
      #   -t "$PROMPT_ARG"                       task description
      MINI_ARGS=(
        --config "/etc/crabcc-agent/mini.yaml"
        --yolo
        --model "${CLAUDE_MODEL:-claude-sonnet-4-6}"
      )
      if [[ -n "${MINI_STEP_LIMIT:-}" ]]; then
        MINI_ARGS+=(--step-limit "${MINI_STEP_LIMIT}")
      fi
      if [[ -n "${MINI_COST_LIMIT:-}" ]]; then
        MINI_ARGS+=(--cost-limit "${MINI_COST_LIMIT}")
      fi
      MINI_ARGS+=(-t "${PROMPT_ARG}")
      echo "[crabcc-agent] dispatch: mini-swe-agent" >&2
      exec mini "${MINI_ARGS[@]}" </dev/null
      ;;

    claude-code|*)
      echo "[crabcc-agent] dispatch: claude-code" >&2
      exec claude code "${CLAUDE_ARGS[@]}" -p "${PROMPT_ARG}" </dev/null
      ;;
  esac
fi

# Fallback: caller passed an explicit command, run it as-is. Useful
# for smoke / debugging.
exec "$@" </dev/null
