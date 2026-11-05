#!/usr/bin/env bash
# Reproduce the five-page drain/GC comparison in pool-gc-findings.md.
#
# Usage:
#   etc/ci/performance/reproduce-drain-gc.sh
#   SKIP_BUILD=1 etc/ci/performance/reproduce-drain-gc.sh
#   CASES="tests/gc/14-many-dom-clones-loop" etc/ci/performance/reproduce-drain-gc.sh
#   GC=1 etc/ci/performance/reproduce-drain-gc.sh     # also count JS_GC calls
#
# Environment:
#   QJS_DIR       servo-quickjs checkout (default: sibling ../servo-quickjs)
#   TARGET_DIR    cargo target dir (default: <repo>/target/drain-stats)
#   OUT           output dir (default: <TARGET_DIR>/repro)
#   CASES         page paths relative to the repo root, without `.html`
#   RUN_SECONDS   per-case run length before SIGKILL (default: 12)
#   SKIP_BUILD=1  use the existing servoshell instead of rebuilding
#   GC=1          extra run per page with QJS_GC_STATS=1
#
# Dromaeo pages are opened directly, so htmlrunner.js runs each test once and
# the process never exits; the timeout is the stop condition.
# tests/gc pages exit on their own (they call window.close()); the timeout is
# only a safety net. Each drain is written immediately, so a SIGKILLed process
# keeps the records written before the kill.
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/../../.." && pwd)
QJS_DIR=${QJS_DIR:-$(cd "$REPO_ROOT/.." && pwd)/servo-quickjs}
TARGET_DIR=${TARGET_DIR:-$REPO_ROOT/target/drain-stats}
OUT=${OUT:-$TARGET_DIR/repro}
# CASES=${CASES:-"
#   tests/dromaeo/dromaeo/web/tests/dom-attr
#   tests/dromaeo/dromaeo/web/tests/dom-query
#   tests/dromaeo/dromaeo/web/tests/dom-traverse
#   tests/dromaeo/dromaeo/web/tests/jslib-modify-jquery
#   tests/dromaeo/dromaeo/web/tests/jslib-modify-prototype
#   tests/gc/14-many-dom-clones-loop
#   tests/gc/15-many-dom-clones-variadic
# "}
CASES=${CASES:-"
  tests/gc/16-scroll-remove-drain
"}
RUN_SECONDS=${RUN_SECONDS:-30}
SKIP_BUILD=${SKIP_BUILD:-0}
GC=${GC:-0}

SERVOSHELL=$TARGET_DIR/debug/servoshell
MERGER=$SCRIPT_DIR/merge_drain_stats.py
PYTHON=${PYTHON:-$REPO_ROOT/.venv/bin/python}
[ -x "$PYTHON" ] || PYTHON=python3

if [ "$SKIP_BUILD" != 1 ]; then
  echo "building servoshell with DRAIN_STATS=1 (in $QJS_DIR)"
  ( cd "$QJS_DIR" && DRAIN_STATS=1 SERVO_TARGET_DIR="$TARGET_DIR" \
      just servo "build --features js-quickjs" )
fi
[ -x "$SERVOSHELL" ] || {
  echo "missing $SERVOSHELL (rebuild without SKIP_BUILD=1)" >&2
  exit 1
}
mkdir -p "$OUT"

for case_path in $CASES; do
  page=$(basename "$case_path")
  html=$REPO_ROOT/$case_path.html
  [ -f "$html" ] || { echo "missing $html" >&2; exit 1; }
  stats=$OUT/qjs-drain-stats-$page.jsonl
  rm -f "$stats"

  echo "running $page for ${RUN_SECONDS}s"
  # The `Killed` notice from the shell is noise here: the SIGKILL is the stop condition.
  status=0
  wall_file=$OUT/wall-$page.txt
  rm -f "$wall_file"
  ( /usr/bin/time -q -f '%e' -o "$wall_file" \
      timeout -s KILL "$RUN_SECONDS" env -u QJS_GC_STATS \
      QJS_DRAIN_STATS_OUT="$stats" \
      "$SERVOSHELL" "$html" --headless > "$OUT/run-$page.log" 2>&1 &
    wait $! ) 2>/dev/null || status=$?
  case $status in
    0|124|137) ;;
    *) echo "  warning: servoshell exited with $status before the timeout" >&2 ;;
  esac
  if [ ! -s "$stats" ]; then
    echo "  warning: no drain record written (the page ran no JS, or died too early)" >&2
    continue
  fi

  if [ "$GC" = 1 ]; then
    ( timeout -s KILL "$RUN_SECONDS" env QJS_GC_STATS=1 QJS_DRAIN_STATS_OUT=/dev/null \
        "$SERVOSHELL" "$html" --headless > "$OUT/gc-$page.log" 2>&1 &
      wait $! ) 2>/dev/null || true
    calls=$(grep -c '^qjs-gc:' "$OUT/gc-$page.log" || true)
    echo "  JS_GC calls: $calls"
  fi
done

if [ -f "$MERGER" ]; then
  "$PYTHON" "$MERGER" "$OUT" --wall
fi

echo "outputs: $OUT"
echo "compare with etc/ci/performance/pool-gc-findings.md"
