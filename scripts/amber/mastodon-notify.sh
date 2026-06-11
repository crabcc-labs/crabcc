#!/usr/bin/env bash
# Compiled from scripts/amber/mastodon-notify.ab
# amber build scripts/amber/mastodon-notify.ab scripts/amber/mastodon-notify.sh --minify
set -euo pipefail

__vis_from_env() {
    local raw="${MASTODON_VISIBILITY:-unlisted}"
    case "${raw}" in
        public)   echo "public"   ;;
        private)  echo "private"  ;;
        direct)   echo "direct"   ;;
        *)        echo "unlisted" ;;
    esac
}

__vis_label() {
    echo "${1}"  # already a string from vis_from_env
}

__result_label() {
    case "${1}" in
        Sent)             echo "sent"              ;;
        SkippedNoToken)   echo "skipped (no token)" ;;
        *)                echo "failed"            ;;
    esac
}

__post() {
    local token="${1}" msg="${2}" vis="${3}"
    if ! curl -sf -X POST "https://social.crabcc.app/api/v1/statuses" \
        -H "Authorization: Bearer ${token}" \
        -F "status=${msg}" \
        -F "visibility=${vis}" \
        -o /dev/null; then
        echo "PostResult::Failed"
        return
    fi
    echo "PostResult::Sent"
}

__notify() {
    local msg="${1}"
    local token="${MASTODON_ACCESS_TOKEN:-}"
    if [ -z "${token}" ]; then
        echo "PostResult::SkippedNoToken"
        return
    fi
    local vis
    vis="$(__vis_from_env)"
    __post "${token}" "${msg}" "${vis}"
}

msg_0="${1:-crabcc notification}"
result_1="$(__notify "${msg_0}")"
label_2="$(__result_label "${result_1}")"

if [ "${result_1}" = "PostResult::Failed" ]; then
    echo "mastodon-notify: ${label_2}" >&2
    exit 1
fi
echo "mastodon-notify: ${label_2}"
