#!/usr/bin/env bash
# Locate the host's Anthropic credential and print it on stdout.
#
# Resolution order (first hit wins):
#   1. $ANTHROPIC_API_KEY in the caller's env
#   2. ~/.claude/.credentials.json — three known shapes:
#        {"anthropicApiKey": "sk-ant-..."}              (legacy)
#        {"claudeAiOauth": {"accessToken": "..."}}      (current OAuth)
#        {"access_token": "..."}                        (older OAuth)
#   3. macOS Keychain — Claude Code stores OAuth there on darwin
#      under the service `Claude Code-credentials`.
#
# Output: the bare token on stdout, nothing else. Non-zero exit when
# no candidate is found (the caller is expected to either print a
# warning and continue without Anthropic, or hard-fail — both are
# legitimate depending on the deployment).
#
# Defensive: the script never logs the token; redacted summaries go
# to stderr.

set -euo pipefail

err() { echo "[extract-anthropic-key] $*" >&2; }

emit() {
  local key="$1"
  local source="$2"
  err "found via ${source} (${#key} chars)"
  printf '%s' "$key"
  exit 0
}

# 1. env wins — operators sometimes pass it explicitly.
if [[ -n "${ANTHROPIC_API_KEY:-}" ]]; then
  emit "$ANTHROPIC_API_KEY" "ANTHROPIC_API_KEY env"
fi

# 2. ~/.claude/.credentials.json
creds="${HOME:-/}/.claude/.credentials.json"
if [[ -r "$creds" ]]; then
  if ! command -v jq >/dev/null 2>&1; then
    err "WARN: $creds present but jq is not installed — skipping"
  else
    for q in '.anthropicApiKey' '.claudeAiOauth.accessToken' '.access_token'; do
      val="$(jq -r "${q} // empty" "$creds" 2>/dev/null || true)"
      if [[ -n "$val" && "$val" != "null" ]]; then
        emit "$val" "credentials.json (${q})"
      fi
    done
  fi
fi

# 3. macOS Keychain
if [[ "$(uname -s)" == "Darwin" ]]; then
  for service in "Claude Code-credentials" "claude-code" "Claude Code"; do
    val="$(security find-generic-password -s "$service" -w 2>/dev/null || true)"
    [[ -z "$val" ]] && continue

    # Keychain may store JSON (OAuth payload) or the raw key. Detect.
    if [[ "${val:0:1}" == "{" ]]; then
      if command -v jq >/dev/null 2>&1; then
        for q in '.anthropicApiKey' '.claudeAiOauth.accessToken' '.access_token'; do
          inner="$(echo "$val" | jq -r "${q} // empty" 2>/dev/null || true)"
          if [[ -n "$inner" && "$inner" != "null" ]]; then
            emit "$inner" "macOS keychain ${service} (${q})"
          fi
        done
      else
        err "WARN: keychain entry for ${service} is JSON but jq is not installed"
      fi
    else
      emit "$val" "macOS keychain ${service} (raw)"
    fi
  done
fi

err "no ANTHROPIC_API_KEY found in env, ~/.claude/.credentials.json, or macOS keychain"
err "options: export ANTHROPIC_API_KEY=..., run \`claude setup-token\`, or sign in via \`claude\`"
exit 1
