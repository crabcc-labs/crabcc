#!/usr/bin/env bash
# langsmith.sh — thin LangSmith API client.
#
# Usage:
#   langsmith.sh get-dataset <dataset-name>
#   langsmith.sh list-examples <dataset-id> [--limit N]
#   langsmith.sh upload-experiment <json-body-file>
#   langsmith.sh ping
#
# Environment:
#   LANGSMITH_API_KEY   (required)
#   LANGSMITH_ENDPOINT  (default: https://eu.api.smith.langchain.com)
#
# All HTTP errors exit 1 with a message on stderr that includes the status
# code and a brief excerpt of the response body.
#
# Logging contract: every operation writes a structured log line to stderr:
#   [langsmith] <iso-ts> <level> <event> key=val ...
# Levels: INFO, WARN, ERROR.

set -uo pipefail

# ── environment ───────────────────────────────────────────────────────────────

# Default endpoint includes the /api/v1 version suffix per LangSmith REST
# docs — without it every dataset/examples/upload-experiment call hits the
# wrong path. Override only if pointing at a non-versioned proxy.
LANGSMITH_ENDPOINT="${LANGSMITH_ENDPOINT:-https://eu.api.smith.langchain.com/api/v1}"

if [[ -z "${LANGSMITH_API_KEY:-}" ]]; then
    echo "[langsmith] $(date -u +%Y-%m-%dT%H:%M:%SZ) ERROR missing_api_key msg=LANGSMITH_API_KEY is not set" >&2
    exit 1
fi

# ── helpers ───────────────────────────────────────────────────────────────────

die() { echo "langsmith.sh: $*" >&2; exit 1; }

log() {
    local level="$1"; shift
    local event="$1"; shift
    # remaining args are key=val pairs
    printf '[langsmith] %s %s %s' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$level" "$event" >&2
    for kv in "$@"; do
        printf ' %s' "$kv" >&2
    done
    printf '\n' >&2
}

# curl wrapper: -fsS = fail on HTTP errors, silent progress bar, show errors.
# On HTTP error curl exits non-zero; we capture stderr and print a richer msg.
# $1 = method (GET or POST)
# $2 = path (e.g. /datasets)
# $3 = optional: path to request body file (for POST)
# $4 = optional: extra query string (already URL-encoded, no leading ?)
api_call() {
    local method="$1"
    local path="$2"
    local body_file="${3:-}"
    local query="${4:-}"

    local url="$LANGSMITH_ENDPOINT$path"
    [[ -n "$query" ]] && url="${url}?${query}"

    # Pass the API key via --config file (chmod 600) instead of -H on the
    # command line, so it does NOT appear in `ps` output of any other user
    # on the host while curl runs.
    local cfg_file
    cfg_file="$(mktemp)"
    chmod 600 "$cfg_file"
    printf 'header = "X-API-Key: %s"\n' "$LANGSMITH_API_KEY" > "$cfg_file"

    local curl_args=(-fsS -X "$method"
        --config "$cfg_file"
        -H "Content-Type: application/json"
        -H "Accept: application/json"
    )
    [[ -n "$body_file" ]] && curl_args+=(-d "@$body_file")

    local http_code
    local response_file
    response_file="$(mktemp)"

    # Run curl with -w to capture the HTTP status separately.
    # -o writes the body to a temp file; -w writes status to stdout.
    http_code="$(curl "${curl_args[@]}" -o "$response_file" -w '%{http_code}' "$url" 2>/tmp/langsmith_curl_err)" || {
        local curl_err
        curl_err="$(cat /tmp/langsmith_curl_err 2>/dev/null || true)"
        rm -f "$response_file" "$cfg_file"
        log ERROR http_error method="$method" path="$path" curl_error="$curl_err"
        echo "langsmith.sh: HTTP request failed: $curl_err" >&2
        exit 1
    }

    local body
    body="$(cat "$response_file")"
    rm -f "$response_file" "$cfg_file"

    if [[ "$http_code" -lt 200 || "$http_code" -ge 300 ]]; then
        local excerpt
        excerpt="$(printf '%s' "$body" | head -c 200)"
        log ERROR http_error method="$method" path="$path" status="$http_code" excerpt="$excerpt"
        echo "langsmith.sh: HTTP $http_code from $method $path — $excerpt" >&2
        exit 1
    fi

    printf '%s\n' "$body"
}

# URL-encode a string (replaces spaces, slashes, etc.)
urlencode() {
    # Use printf + sed; avoid python/perl deps.
    local raw="$1"
    printf '%s' "$raw" | jq -Rr '@uri'
}

# ── subcommands ───────────────────────────────────────────────────────────────

cmd_get_dataset() {
    [[ $# -ge 1 ]] || die "usage: get-dataset <dataset-name>"
    local name="$1"
    local encoded
    encoded="$(urlencode "$name")"

    log INFO dataset_fetch_start name="$name"
    local resp
    resp="$(api_call GET /datasets "" "name=${encoded}")"

    # The response is an array; find the first matching entry.
    local result
    result="$(printf '%s\n' "$resp" | jq -c --arg n "$name" \
        '[.[] | select(.name == $n)] | .[0] // empty')" || true

    if [[ -z "$result" ]]; then
        log ERROR dataset_not_found name="$name"
        echo "langsmith.sh: dataset '$name' not found" >&2
        exit 1
    fi

    log INFO dataset_fetch_done name="$name" \
        id="$(printf '%s' "$result" | jq -r '.id // "?"')" \
        example_count="$(printf '%s' "$result" | jq -r '.example_count // "?"')"
    printf '%s\n' "$result"
}

cmd_list_examples() {
    [[ $# -ge 1 ]] || die "usage: list-examples <dataset-id> [--limit N]"
    local dataset_id="$1"; shift

    local user_limit=""
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --limit) user_limit="$2"; shift 2 ;;
            *) die "unknown flag: $1" ;;
        esac
    done

    # Paginate via offset/limit. LangSmith caps page size around 100; iterate
    # until a page comes back smaller than the requested page size, then stop.
    # Without this, datasets larger than one page were silently truncated and
    # import-dataset.sh still reported success.
    local page_size=100
    local offset=0
    local total=0
    local accumulated="[]"

    log INFO examples_fetch_start dataset_id="$dataset_id" \
        limit="${user_limit:-all}" page_size="$page_size"

    while :; do
        # If the user passed --limit, cap remaining slots so we don't overshoot.
        local this_size="$page_size"
        if [[ -n "$user_limit" ]]; then
            local remaining=$(( user_limit - total ))
            (( remaining <= 0 )) && break
            (( remaining < page_size )) && this_size="$remaining"
        fi

        local query="dataset=${dataset_id}&limit=${this_size}&offset=${offset}"
        local page
        page="$(api_call GET /examples "" "$query")"

        local got
        got="$(printf '%s\n' "$page" | jq 'length')"
        (( got == 0 )) && break

        accumulated="$(jq -nc --argjson a "$accumulated" --argjson b "$page" '$a + $b')"
        total=$(( total + got ))
        offset=$(( offset + got ))

        log INFO examples_page dataset_id="$dataset_id" offset="$offset" got="$got" total="$total"

        # Short page → server has no more rows.
        (( got < this_size )) && break
    done

    log INFO examples_fetch_done dataset_id="$dataset_id" count="$total"
    printf '%s\n' "$accumulated"
}

cmd_upload_experiment() {
    [[ $# -ge 1 ]] || die "usage: upload-experiment <json-body-file>"
    local body_file="$1"

    [[ -f "$body_file" ]] || die "body file not found: $body_file"

    # Validate JSON before posting.
    jq -e . "$body_file" >/dev/null 2>&1 \
        || die "body file is not valid JSON: $body_file"

    local exp_name
    exp_name="$(jq -r '.experiment_name // "unknown"' "$body_file")"
    log INFO upload_start experiment_name="$exp_name"

    local resp
    resp="$(api_call POST /datasets/upload-experiment "$body_file")"

    # Response shape per LangSmith REST docs:
    #   { "experiment": { "id": ..., "name": ..., "run_count": ... },
    #     "dataset":    { "id": ..., "name": ..., "example_count": ... } }
    # Fall back to top-level keys for forward-compat in case the shape flips.
    local exp_id dataset_id
    exp_id="$(printf '%s\n' "$resp" | jq -r '.experiment.id // .experiment_id // empty')"
    dataset_id="$(printf '%s\n' "$resp" | jq -r '.dataset.id // .dataset_id // empty')"

    log INFO upload_ok experiment_id="$exp_id" dataset_id="$dataset_id"
    printf '%s\n' "$resp" | jq -c '{experiment_id: (.experiment.id // .experiment_id), dataset_id: (.dataset.id // .dataset_id)}'
}

cmd_ping() {
    log INFO ping_start endpoint="$LANGSMITH_ENDPOINT"
    local resp
    resp="$(api_call GET /info "" "")" || exit 1
    log INFO ping_ok
    printf '%s\n' "$resp"
}

# ── dispatch ──────────────────────────────────────────────────────────────────

[[ $# -ge 1 ]] || { echo "usage: langsmith.sh <subcommand> [args...]" >&2; exit 1; }
SUBCMD="$1"; shift

case "$SUBCMD" in
    get-dataset)        cmd_get_dataset "$@" ;;
    list-examples)      cmd_list_examples "$@" ;;
    upload-experiment)  cmd_upload_experiment "$@" ;;
    ping)               cmd_ping ;;
    *) die "unknown subcommand: $SUBCMD" ;;
esac
