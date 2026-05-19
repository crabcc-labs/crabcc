#!/usr/bin/env bash
# Deprecated: crabcc-telegram removed. Use the HITL stack instead.
set -euo pipefail
cd "$(dirname "$0")/.."
exec task hitl:down
