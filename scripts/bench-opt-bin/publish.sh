#!/usr/bin/env bash
# publish.sh — fan a completed bench-opt-bin run out to durable sinks.
#
# Every sink is env-gated and a no-op when unconfigured, so this is safe to
# call unconditionally at the end of provision-ovh.sh (or by hand). Run it
# from a machine that has the relevant credentials (the OVH box / your
# laptop) — NOT from inside an ephemeral agent container, which won't have
# ~/.composio or a valid COMPOSIO_API_KEY.
#
# Usage:  scripts/bench-opt-bin/publish.sh <run-dir> [<tarball>]
#   <run-dir>  a bench/results/run-<host>-<stamp>/ directory
#   <tarball>  optional; defaults to <run-dir>.tar.gz
#
# Sinks & their env:
#   1. _bench-results repo (system of record, Git LFS for *.tar.gz/*.svg)
#        BENCH_RESULTS_REPO   git remote, e.g. git@github.com:org/_bench-results.git
#   2. Discord notification via channel incoming webhook
#        DISCORD_WEBHOOK_URL     Server Settings → Integrations → Webhooks → New
#      (Composio's Discord connection is read-only user-OAuth2 — no send action —
#       so a webhook is the working path; no Composio/bot token needed to post.)
#   3. Google Drive upload via Composio REST
#        COMPOSIO_API_KEY        (sanitized below — keeps [A-Za-z0-9_-], strips
#                                 the smart-quote bytes secret stores inject)
#        COMPOSIO_DRIVE_ACCOUNT    connected-account id (Drive)
#        DRIVE_FOLDER_ID           optional parent folder
#        DRIVE_TOOL_SLUG           default GOOGLEDRIVE_UPLOAD_FILE
#
# Composio note: tool slugs/argument names vary by toolkit version; they're
# overridable via env and the API response is printed so a mismatch is
# obvious. The git/LFS sink has no external dependency and is the reliable one.
set -uo pipefail

RUN_DIR="${1:?usage: publish.sh <run-dir> [tarball]}"
TARBALL="${2:-${RUN_DIR%/}.tar.gz}"
RUN_ID="$(basename "${RUN_DIR%/}")"
COMPOSIO_BASE="${COMPOSIO_BASE:-https://backend.composio.dev/api/v3}"
REPORT="$RUN_DIR/REPORT.md"

# Composio keys frequently arrive wrapped in smart-quotes from secret stores;
# keep only the bytes a real key can contain. NOTE the hyphen: Composio keys
# look like ak_Kz8g8l5eQ8-lvUTV2AzL — dropping `-` silently corrupts the key
# (→ 401), so the class is [A-Za-z0-9_-], not [A-Za-z0-9_].
CKEY="$(printf '%s' "${COMPOSIO_API_KEY:-}" | tr -cd 'A-Za-z0-9_-')"

summary() {  # first useful lines of the report for a notification body
  [ -f "$REPORT" ] && sed -n '1,18p' "$REPORT" || echo "bench-opt-bin run $RUN_ID"
}

# --- 1. _bench-results repo + LFS -------------------------------------------
if [ -n "${BENCH_RESULTS_REPO:-}" ]; then
  echo ">> [sink] _bench-results repo: $BENCH_RESULTS_REPO"
  tmp="$(mktemp -d)"
  if git clone --depth 1 "$BENCH_RESULTS_REPO" "$tmp" 2>/dev/null; then
    ( cd "$tmp"
      command -v git-lfs >/dev/null && git lfs install --local >/dev/null 2>&1 || true
      if [ ! -f .gitattributes ] || ! grep -q 'tar.gz' .gitattributes; then
        printf '%s\n' '*.tar.gz filter=lfs diff=lfs merge=lfs -text' \
                      '*.svg filter=lfs diff=lfs merge=lfs -text' >> .gitattributes
        git add .gitattributes
      fi
      mkdir -p "runs/$RUN_ID"
      cp -r "$RUN_DIR"/. "runs/$RUN_ID"/
      [ -f "$TARBALL" ] && cp "$TARBALL" "runs/$RUN_ID/"
      git add "runs/$RUN_ID" .gitattributes
      git commit -q -m "opt-bin run $RUN_ID" \
        && for i in 1 2 3 4; do git push && break || sleep $((2**i)); done
    ) && echo "   pushed runs/$RUN_ID" || echo "   ! git push failed"
  else
    echo "   ! clone failed — check BENCH_RESULTS_REPO + credentials"
  fi
  rm -rf "$tmp"
fi

# --- Composio execute helper -------------------------------------------------
composio_exec() {  # $1=slug  $2=account_id  $3=arguments-json
  curl -s -m 40 -X POST "$COMPOSIO_BASE/tools/execute/$1" \
    -H "x-api-key: $CKEY" -H "Content-Type: application/json" \
    -d "{\"connected_account_id\":\"$2\",\"arguments\":$3}"
}

# --- 2. Discord notification (incoming webhook) ------------------------------
# NOTE: Composio's Discord connection is read-only user-OAuth2 (GET_MY_USER,
# LIST_MY_GUILDS, … — no send action), and a user token can't post to
# channels anyway. Notifications therefore go through a channel *incoming
# webhook*: Discord → Server Settings → Integrations → Webhooks → New Webhook
# → Copy URL → export DISCORD_WEBHOOK_URL. No Composio/bot needed to send.
if [ -n "${DISCORD_WEBHOOK_URL:-}" ]; then
  echo ">> [sink] Discord webhook notify"
  payload="$(python3 - "$(summary)" "$RUN_ID" <<'PY'
import json,sys
print(json.dumps({"username": "bench-opt-bin",
                  "content": "**bench-opt-bin run complete** — `%s`\n```\n%s\n```" % (sys.argv[2], sys.argv[1][:1700])}))
PY
)"
  code=$(curl -s -o /dev/null -w '%{http_code}' -m 30 -X POST "$DISCORD_WEBHOOK_URL" \
    -H "Content-Type: application/json" -d "$payload")
  echo "   discord webhook → HTTP $code"
fi

# --- 3. Google Drive upload --------------------------------------------------
if [ -n "$CKEY" ] && [ -n "${COMPOSIO_DRIVE_ACCOUNT:-}" ] && [ -f "$TARBALL" ]; then
  echo ">> [sink] Google Drive upload ($(basename "$TARBALL"))"
  args="$(python3 - "$TARBALL" "${DRIVE_FOLDER_ID:-}" <<'PY'
import json,sys
a={"file_to_upload": sys.argv[1]}
if sys.argv[2]: a["parent_id"]=sys.argv[2]
print(json.dumps(a))
PY
)"
  resp="$(composio_exec "${DRIVE_TOOL_SLUG:-GOOGLEDRIVE_UPLOAD_FILE}" "$COMPOSIO_DRIVE_ACCOUNT" "$args")"
  echo "   composio: $(printf '%s' "$resp" | head -c 300)"
fi

echo ">> publish done for $RUN_ID"
