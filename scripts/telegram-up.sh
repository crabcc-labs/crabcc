#!/usr/bin/env bash
# telegram-up.sh — idempotent bring-up for the crabcc-telegram bot.
#
# Pipeline:
#   1. Ensure a healthy cloudflared quick-tunnel exists pointing at the
#      local `crabcc serve` (default port 7878 — matches the CLI default).
#      Reuse if running + reachable; otherwise spawn via the existing
#      `scripts/cloudflared-tunnel.sh` shim.
#   2. Validate apps/crabcc-telegram/.env has a non-empty TELEGRAM_BOT_TOKEN.
#   3. Build the docker image — skip if the cached image is newer than the
#      bot's source tree.
#   4. Run the container (bridge network + host.docker.internal mapping
#      so the in-container bot can reach the host's `crabcc serve`).
#      If a container already exists with the same CRABCC_PUBLIC_URL,
#      leave it alone; otherwise restart with the fresh URL.
#   5. Tail container logs ~10 s for the "✓ Telegram getMe ok" line.
#   6. Persist {pid, url, container_id, started_at} to
#      .crabcc/telegram-up.json so subsequent runs short-circuit.
#
# Hard rules:
#   - set -euo pipefail; trap unwinds partial state.
#   - Never log TELEGRAM_BOT_TOKEN; redact when echoing .env.
#   - All paths absolute or anchored to SCRIPT_DIR.

set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)
TELEGRAM_DIR="$REPO_ROOT/apps/crabcc-telegram"
ENV_FILE="$TELEGRAM_DIR/.env"
DOCKERFILE="$TELEGRAM_DIR/Dockerfile"
STATE_DIR="$REPO_ROOT/.crabcc"
STATE_FILE="$STATE_DIR/telegram-up.json"
TUNNEL_SCRIPT="$SCRIPT_DIR/cloudflared-tunnel.sh"
TUNNEL_LOG="$HOME/Library/Logs/Crabcc/cloudflared.log"
TUNNEL_URL_FILE="$HOME/.crabcc/cloudflared.url"
TUNNEL_PID_FILE="$HOME/.crabcc/cloudflared.pid"

CONTAINER_NAME="crabcc-telegram"
IMAGE_TAG="crabcc-telegram:dev"
LOCAL_SERVE_PORT="${CRABCC_SERVE_PORT:-7878}"

mkdir -p "$STATE_DIR"

# ── logging helpers (no secrets, ever) ───────────────────────────────────
log()  { printf '\033[36m[telegram-up]\033[0m %s\n' "$*" >&2; }
ok()   { printf '\033[32m[telegram-up]\033[0m ✓ %s\n' "$*" >&2; }
warn() { printf '\033[33m[telegram-up]\033[0m ⚠ %s\n' "$*" >&2; }
die()  { printf '\033[31m[telegram-up]\033[0m ✗ %s\n' "$*" >&2; exit 1; }

# Cleanup hook for partial bring-ups. Only fires on non-zero exit.
PARTIAL_CONTAINER=""
cleanup() {
    local rc=$?
    if [[ $rc -ne 0 && -n "$PARTIAL_CONTAINER" ]]; then
        warn "partial bring-up — removing container $PARTIAL_CONTAINER"
        docker rm -f "$PARTIAL_CONTAINER" >/dev/null 2>&1 || true
    fi
    exit $rc
}
trap cleanup EXIT

# ── 1. tunnel ─────────────────────────────────────────────────────────────

# Health-check a tunnel URL. Returns 0 if it responds with any HTTP code
# (200/3xx/4xx all prove the tunnel is forwarding); 1 if connection fails.
tunnel_reachable() {
    local url="$1"
    local code
    code=$(curl -sS -o /dev/null -w '%{http_code}' --max-time 5 "$url/api/bootstrap" 2>/dev/null || echo "000")
    [[ "$code" != "000" ]]
}

# True if cloudflared is running AND its url-file exists AND the URL
# resolves (any HTTP response from /api/bootstrap; 502 is fine — it just
# means the host's `crabcc serve` is down, but the tunnel itself is up).
tunnel_healthy() {
    [[ -f "$TUNNEL_PID_FILE" ]] || return 1
    local pid
    pid=$(cat "$TUNNEL_PID_FILE")
    kill -0 "$pid" 2>/dev/null || return 1
    [[ -f "$TUNNEL_URL_FILE" ]] || return 1
    local url
    url=$(cat "$TUNNEL_URL_FILE")
    [[ -n "$url" ]] || return 1
    tunnel_reachable "$url"
}

ensure_tunnel() {
    if tunnel_healthy; then
        local url
        url=$(cat "$TUNNEL_URL_FILE")
        ok "tunnel healthy (pid $(cat "$TUNNEL_PID_FILE"), $url)"
        echo "$url"
        return 0
    fi

    # Tunnel state is dirty (running but stale, or url is gone). Force
    # a clean start. The cloudflared-tunnel.sh script already handles
    # the "already running" case gracefully when state is consistent;
    # only reach here when it's not.
    if [[ -f "$TUNNEL_PID_FILE" ]] && ! kill -0 "$(cat "$TUNNEL_PID_FILE")" 2>/dev/null; then
        warn "stale cloudflared pid — clearing"
        rm -f "$TUNNEL_PID_FILE" "$TUNNEL_URL_FILE"
    fi

    log "starting cloudflared tunnel → http://localhost:${LOCAL_SERVE_PORT}"
    PORT="$LOCAL_SERVE_PORT" bash "$TUNNEL_SCRIPT" start >/dev/null

    # The shim writes URL_FILE once it parses the trycloudflare URL.
    # Wait up to 30 s for the tunnel to also be reachable end-to-end.
    local deadline=$(( $(date +%s) + 30 ))
    while (( $(date +%s) < deadline )); do
        if [[ -f "$TUNNEL_URL_FILE" ]] && tunnel_reachable "$(cat "$TUNNEL_URL_FILE")"; then
            local url
            url=$(cat "$TUNNEL_URL_FILE")
            ok "tunnel up: $url"
            echo "$url"
            return 0
        fi
        sleep 1
    done

    die "tunnel did not become reachable within 30s — see $TUNNEL_LOG"
}

# ── 2. token check ────────────────────────────────────────────────────────

check_token() {
    if [[ ! -r "$ENV_FILE" ]]; then
        die ".env missing at $ENV_FILE — run: bash $TELEGRAM_DIR/setup.sh (or 'task -d $TELEGRAM_DIR env-init')"
    fi
    # Extract without ever echoing the value. `grep -E` + `cut` keeps the
    # token in a local var that we only check for non-empty + length.
    local token
    token=$(grep -E '^TELEGRAM_BOT_TOKEN=' "$ENV_FILE" | head -1 | cut -d= -f2- | tr -d '"\r' | xargs || true)
    if [[ -z "$token" ]]; then
        die "TELEGRAM_BOT_TOKEN missing in $ENV_FILE — run: task -d $TELEGRAM_DIR env-init"
    fi
    # Never log the token; len + prefix is enough breadcrumb for ops.
    ok "TELEGRAM_BOT_TOKEN present (len=${#token}, prefix=${token:0:6}…)"
}

# ── 3. image build (skip-if-fresh) ───────────────────────────────────────

# True if the image exists AND its CreatedAt timestamp is newer than the
# newest mtime under src/, plus Cargo.toml + Dockerfile (the inputs that
# would meaningfully change the binary).
image_is_fresh() {
    local image_ts
    image_ts=$(docker image inspect "$IMAGE_TAG" --format '{{.Created}}' 2>/dev/null || true)
    [[ -n "$image_ts" ]] || return 1

    # Convert ISO-8601 → epoch. macOS `date -j -f` accepts the trimmed
    # form; we strip nanos + 'Z' to keep both BSD and GNU date happy.
    local trimmed image_epoch
    trimmed=${image_ts%.*}
    trimmed=${trimmed%Z}
    if image_epoch=$(date -j -u -f '%Y-%m-%dT%H:%M:%S' "$trimmed" '+%s' 2>/dev/null); then
        :
    elif image_epoch=$(date -d "$image_ts" '+%s' 2>/dev/null); then
        :
    else
        # Date parse failed → safest to rebuild.
        return 1
    fi

    # Newest source mtime. Use find -print0 + while-read for portability.
    local newest_src=0
    while IFS= read -r -d '' f; do
        local m
        if m=$(stat -f '%m' "$f" 2>/dev/null); then :;
        elif m=$(stat -c '%Y' "$f" 2>/dev/null); then :;
        else continue; fi
        (( m > newest_src )) && newest_src=$m
    done < <(find "$TELEGRAM_DIR/src" "$TELEGRAM_DIR/Cargo.toml" "$TELEGRAM_DIR/Cargo.lock" "$TELEGRAM_DIR/Dockerfile" -type f -print0 2>/dev/null)

    (( image_epoch >= newest_src ))
}

build_image() {
    if image_is_fresh; then
        ok "image $IMAGE_TAG is fresh — skip build"
        return 0
    fi
    log "building $IMAGE_TAG (this may take a few minutes on a cold cache)"
    # Build context is the bot crate, not repo root — Cargo.toml here is
    # standalone (workspace = []) per the teloxide-ICE workaround.
    docker build -f "$DOCKERFILE" -t "$IMAGE_TAG" "$TELEGRAM_DIR" >&2
    ok "image built: $IMAGE_TAG"
}

# ── 4. container run (idempotent) ────────────────────────────────────────

container_state() {
    # `docker inspect` exits non-zero when the container doesn't exist;
    # in that case `--format` may emit nothing, so we explicitly
    # short-circuit to "missing" *and* trim trailing whitespace from
    # the success path so callers can string-compare cleanly.
    local out
    out=$(docker inspect "$CONTAINER_NAME" --format '{{.State.Status}}' 2>/dev/null) || out="missing"
    printf '%s' "$out" | tr -d '[:space:]'
}

container_env_var() {
    # Docker preserves *all* env entries — both --env-file and -e flags.
    # When both set the same key, the runtime exposes the LAST one to the
    # process, so we mirror that with `tail -1`. Without this, a stale
    # value baked into apps/crabcc-telegram/.env would mask the fresh -e
    # override and trigger spurious "stale URL" restart loops.
    local var="$1"
    docker inspect "$CONTAINER_NAME" --format "{{range .Config.Env}}{{println .}}{{end}}" 2>/dev/null \
        | grep -E "^${var}=" | tail -1 | cut -d= -f2- | tr -d '\r' || true
}

run_container() {
    local public_url="$1"
    local state
    state=$(container_state)

    if [[ "$state" == "running" ]]; then
        local current_url
        current_url=$(container_env_var CRABCC_PUBLIC_URL)
        if [[ "$current_url" == "$public_url" ]]; then
            ok "container $CONTAINER_NAME already running with current tunnel URL"
            return 0
        fi
        warn "container running with stale CRABCC_PUBLIC_URL — restarting"
        docker rm -f "$CONTAINER_NAME" >/dev/null
    elif [[ "$state" != "missing" ]]; then
        # exited / created / paused — remove and re-create cleanly.
        warn "container in state '$state' — removing"
        docker rm -f "$CONTAINER_NAME" >/dev/null
    fi

    log "starting container $CONTAINER_NAME"
    PARTIAL_CONTAINER="$CONTAINER_NAME"
    # --add-host gives the bridged container a DNS name for the host so
    # the in-container bot can reach `crabcc serve` running on the host.
    # --env-file at run time keeps the token out of the image; the
    # CRABCC_* envs override anything stale in .env.
    docker run -d \
        --name "$CONTAINER_NAME" \
        --restart=unless-stopped \
        --add-host=host.docker.internal:host-gateway \
        --env-file "$ENV_FILE" \
        -e "CRABCC_PUBLIC_URL=$public_url" \
        -e "CRABCC_SERVE_URL=http://host.docker.internal:${LOCAL_SERVE_PORT}" \
        "$IMAGE_TAG" >/dev/null
    PARTIAL_CONTAINER=""

    ok "container started: $CONTAINER_NAME"
}

# ── 5. wait for "getMe ok" log line ──────────────────────────────────────

verify_bot() {
    log "waiting up to 15 s for getMe success line in container logs…"
    local deadline=$(( $(date +%s) + 15 ))
    local found=""
    while (( $(date +%s) < deadline )); do
        # `getMe ok` is the bot's success-after-token-validation log line.
        # If it never appears the token is bad / Telegram is unreachable.
        if docker logs "$CONTAINER_NAME" 2>&1 | grep -q 'Telegram getMe ok'; then
            found="yes"
            break
        fi
        if docker logs "$CONTAINER_NAME" 2>&1 | grep -q 'getMe failed'; then
            warn "container reported getMe FAILED — token / network issue"
            docker logs --tail 20 "$CONTAINER_NAME" >&2
            return 1
        fi
        sleep 1
    done
    if [[ -z "$found" ]]; then
        warn "did not see 'getMe ok' within 15 s — bot may still be starting"
        docker logs --tail 10 "$CONTAINER_NAME" >&2
    else
        # tracing's structured fields look like `username=Some("foo")` —
        # grep both Some(...) and bare forms so we always have a name to print.
        local who
        who=$(docker logs "$CONTAINER_NAME" 2>&1 \
            | grep -oE 'username=Some\("[^"]+"\)' \
            | sed -E 's/username=Some\("([^"]+)"\)/\1/' \
            | head -1 || true)
        ok "bot online${who:+ — @$who}"
    fi
}

# ── 6. persist state ─────────────────────────────────────────────────────

write_state() {
    local public_url="$1"
    local container_id pid
    container_id=$(docker inspect "$CONTAINER_NAME" --format '{{.Id}}' 2>/dev/null | cut -c1-12 || echo "?")
    pid=$(cat "$TUNNEL_PID_FILE" 2>/dev/null || echo "?")
    cat > "$STATE_FILE" <<EOF
{
  "tunnel_url": "$public_url",
  "tunnel_pid": $pid,
  "tunnel_log": "$TUNNEL_LOG",
  "container_name": "$CONTAINER_NAME",
  "container_id": "$container_id",
  "image": "$IMAGE_TAG",
  "serve_port": $LOCAL_SERVE_PORT,
  "started_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
EOF
    ok "state written → $STATE_FILE"
}

# ── short-circuit: already up? ───────────────────────────────────────────

already_up() {
    [[ -f "$STATE_FILE" ]] || return 1
    tunnel_healthy || return 1
    [[ "$(container_state)" == "running" ]] || return 1
    # Cross-check: state file's tunnel_url must match the current URL_FILE.
    local saved current
    saved=$(grep -oE '"tunnel_url"[[:space:]]*:[[:space:]]*"[^"]+"' "$STATE_FILE" | sed -E 's/.*"([^"]+)"$/\1/')
    current=$(cat "$TUNNEL_URL_FILE" 2>/dev/null || true)
    [[ -n "$saved" && "$saved" == "$current" ]] || return 1
    # And the container's CRABCC_PUBLIC_URL must agree.
    local container_url
    container_url=$(container_env_var CRABCC_PUBLIC_URL)
    [[ "$container_url" == "$current" ]]
}

# ── main ─────────────────────────────────────────────────────────────────

main() {
    if already_up; then
        local url
        url=$(cat "$TUNNEL_URL_FILE")
        ok "already up — tunnel=$url, container=$CONTAINER_NAME"
        echo "$url"
        return 0
    fi

    check_token
    local public_url
    public_url=$(ensure_tunnel)
    build_image
    run_container "$public_url"
    verify_bot
    write_state "$public_url"

    cat <<EOF >&2

────────────────────────────────────────
✓ telegram bot up
  tunnel:    $public_url
  container: $CONTAINER_NAME
  state:     $STATE_FILE
  logs:      docker logs -f $CONTAINER_NAME
────────────────────────────────────────
EOF
    echo "$public_url"
}

main "$@"
