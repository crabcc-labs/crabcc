#!/usr/bin/env bash
# crabcc-core fuzzing matrix — libFuzzer fork-mode, every target at once,
# scaled to all cores. Built for an unattended ~45 min run on a dedicated VM.
#
# Fork mode (`-fork=N -ignore_crashes=1`) is what makes this a *matrix* rather
# than the laptop quick-run: each target self-parallelizes into N worker
# processes AND keeps fuzzing after it finds a crash, so one target can
# surface many distinct crashes in a single run instead of dying on the first.
#
# Usage:  scripts/fuzz/matrix.sh [DURATION_SECONDS] [REPO_ROOT]
#   DURATION_SECONDS  wall time per target (default 2700 = 45 min)
#   REPO_ROOT         repo checkout (default: inferred from this script)
set -uo pipefail

DURATION="${1:-2700}"
REPO_ROOT="${2:-$(cd "$(dirname "$0")/../.." && pwd)}"
CORE_DIR="$REPO_ROOT/crates/crabcc-core"
cd "$CORE_DIR" || { echo "no crabcc-core at $CORE_DIR" >&2; exit 2; }

command -v cargo >/dev/null || { echo "cargo missing — run provision-ovh.sh first" >&2; exit 2; }
NPROC="$(getconf _NPROCESSORS_ONLN 2>/dev/null || nproc 2>/dev/null || sysctl -n hw.ncpu)"
TRIPLE="$(rustc -Vv | sed -n 's/^host: //p')"
BINDIR="fuzz/target/$TRIPLE/release"
STAMP="$(date -u +%Y%m%dT%H%M%SZ 2>/dev/null || echo run)"
OUT="$CORE_DIR/fuzz/matrix-$STAMP"
mkdir -p "$OUT/logs"

echo "[matrix] building all targets (ASan) ..."
cargo +nightly fuzz build || exit 1

# Portable target enumeration (bash 3.2 on macOS has no mapfile).
TARGETS=()
while IFS= read -r line; do TARGETS+=("$line"); done < <(cargo +nightly fuzz list)
NT="${#TARGETS[@]}"
[ "$NT" -gt 0 ] || { echo "no fuzz targets found" >&2; exit 2; }

# Spread workers across cores: fork-per-target * NT ~= NPROC.
FORK=$(( NPROC / NT )); [ "$FORK" -lt 1 ] && FORK=1
echo "[matrix] nproc=$NPROC targets=$NT fork/target=$FORK (~$((FORK*NT)) workers) duration=${DURATION}s"

pids=(); names=()
for t in "${TARGETS[@]}"; do
  mkdir -p "fuzz/corpus/$t" "fuzz/artifacts/$t"
  "$BINDIR/$t" -fork="$FORK" -ignore_crashes=1 -max_total_time="$DURATION" \
      -print_final_stats=1 -artifact_prefix="fuzz/artifacts/$t/" \
      "fuzz/corpus/$t" > "$OUT/logs/$t.log" 2>&1 &
  pids+=($!); names+=("$t")
done
echo "[matrix] launched $NT targets; waiting up to ${DURATION}s ..."
for p in "${pids[@]}"; do wait "$p"; done

REPORT="$OUT/REPORT.md"
{
  echo "# crabcc fuzz matrix report — $STAMP"
  echo
  echo "- host: \`$(uname -srm)\`  nproc=$NPROC"
  echo "- per-target: ${DURATION}s, fork=$FORK ($NT targets, ~$((FORK*NT)) workers)"
  echo
  echo "| target | unique crashes | last cov | execs |"
  echo "|---|---|---|---|"
  for t in "${TARGETS[@]}"; do
    n=$(find "fuzz/artifacts/$t" -type f -name 'crash-*' 2>/dev/null | wc -l | tr -d ' ')
    cov=$(grep -Eo 'cov: [0-9]+' "$OUT/logs/$t.log" | tail -1)
    ex=$(grep -Eo 'stat::number_of_executed_units: [0-9]+' "$OUT/logs/$t.log" | tail -1 | grep -Eo '[0-9]+$')
    [ -z "$ex" ] && ex=$(grep -Eo '#[0-9]+' "$OUT/logs/$t.log" | tail -1 | tr -d '#')  # fork-mode fallback
    echo "| \`$t\` | $n | ${cov:-?} | ${ex:-?} |"
  done
  echo
  echo "## crash artifacts"
  echo '```'
  find fuzz/artifacts -type f -name 'crash-*' 2>/dev/null || echo "none"
  echo '```'
  echo
  echo "Reproduce any crash with:"
  echo '```'
  echo "cd crates/crabcc-core && $BINDIR/<target> <artifact-path>"
  echo '```'
} | tee "$REPORT"

tar czf "$OUT/bundle.tar.gz" -C "$CORE_DIR/fuzz" artifacts corpus 2>/dev/null || true
echo "[matrix] report: $REPORT"
echo "[matrix] bundle: $OUT/bundle.tar.gz  (scp this back for triage)"
