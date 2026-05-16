#!/usr/bin/env bash
# resolve-manifest.sh — read an agent manifest TOML and print key=value lines.
#
# Usage: resolve-manifest.sh <agent-name>
#
# Output (one key=value per line, suitable for `source` or `export`):
#   AGENT_NAME=swe-build
#   MODEL=openrouter/deepseek/deepseek-v4-pro
#   PROMPT_FILE=agents/prompts/swe-build.md
#   MANIFEST_SHA=<git-sha-of-the-toml>
#
# Exit codes:
#   0  OK
#   1  missing argument, manifest not found, or required field absent

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel 2>/dev/null)"
if [ -z "$REPO" ]; then
    echo "resolve-manifest.sh must live inside a git repo" >&2
    exit 1
fi

if [ $# -lt 1 ] || [ -z "$1" ]; then
    echo "usage: resolve-manifest.sh <agent-name>" >&2
    exit 1
fi

AGENT_NAME="$1"
MANIFEST_REL="agents/${AGENT_NAME}.toml"
MANIFEST="$REPO/$MANIFEST_REL"

if [ ! -f "$MANIFEST" ]; then
    echo "resolve-manifest.sh: manifest not found: $MANIFEST" >&2
    exit 1
fi

# --- parse required fields via grep/sed (no external toml parser) ---

# Strip inline comments and extract the value after the '=' on matching lines.
# Handles both quoted ("value") and unquoted (value) forms.
_toml_field() {
    local key="$1"
    local file="$2"
    # Match lines like: key = "value" or key = value (ignoring leading whitespace)
    grep -m1 "^[[:space:]]*${key}[[:space:]]*=" "$file" \
        | sed 's/^[^=]*=[[:space:]]*//' \
        | sed 's/[[:space:]]*#.*//' \
        | sed 's/^"\(.*\)"$/\1/' \
        | tr -d '\r'
}

MODEL="$(_toml_field model "$MANIFEST")"
PROMPT_FILE="$(_toml_field file "$MANIFEST")"
NAME_IN_FILE="$(_toml_field name "$MANIFEST")"

# Validate required fields
MISSING=""
[ -z "$NAME_IN_FILE" ] && MISSING="${MISSING} name"
[ -z "$MODEL"         ] && MISSING="${MISSING} model"
[ -z "$PROMPT_FILE"   ] && MISSING="${MISSING} agent.prompt.file"

if [ -n "$MISSING" ]; then
    echo "resolve-manifest.sh: $MANIFEST is missing required fields:${MISSING}" >&2
    exit 1
fi

# Derive the manifest SHA from git's object store.
# This reflects the committed content; use HEAD so callers get the stable version.
MANIFEST_SHA="$(git -C "$REPO" rev-parse "HEAD:${MANIFEST_REL}" 2>/dev/null)" || MANIFEST_SHA=""
if [ -z "$MANIFEST_SHA" ]; then
    # File exists on disk but is not yet committed — hash the working copy.
    MANIFEST_SHA="$(git -C "$REPO" hash-object "$MANIFEST" 2>/dev/null || true)"
fi
if [ -z "$MANIFEST_SHA" ]; then
    echo "resolve-manifest.sh: could not derive SHA for $MANIFEST_REL" >&2
    exit 1
fi

printf 'AGENT_NAME=%s\n' "$AGENT_NAME"
printf 'MODEL=%s\n'       "$MODEL"
printf 'PROMPT_FILE=%s\n' "$PROMPT_FILE"
printf 'MANIFEST_SHA=%s\n' "$MANIFEST_SHA"
