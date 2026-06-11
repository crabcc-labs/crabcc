#!/usr/bin/env bash
# bench-seq.sh — sequential baseline (Bash side of amber-vs-bash comparison)
# 10 tasks each sleeping 0.1s sequentially.
# Expected wall time: ~1.0s
set -euo pipefail
for i in $(seq 1 10); do
    sleep 0.1
done
