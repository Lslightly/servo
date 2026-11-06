#!/usr/bin/env bash
# Reproduce the drain/GC comparison in pool-gc-findings.md.
#
# Usage:
#   etc/ci/performance/reproduce-drain-gc.sh
#   SKIP_BUILD=1 etc/ci/performance/reproduce-drain-gc.sh
#   CASES="tests/gc/14-many-dom-clones-loop" etc/ci/performance/reproduce-drain-gc.sh
#
# Each page runs once with both stats enabled: drain time and GC time do not
# overlap, so one pass feeds both plots. The drain and GC JSONL files, the CSVs
# from merge_drain_stats.py, and one PNG/SVG pair per page and plot land in $OUT.
#
# Environment:
#   QJS_DIR       servo-quickjs checkout (default: <repo>/servo-quickjs,
#                 else the sibling ../servo-quickjs)
#   TARGET_DIR    cargo target dir (default: <repo>/target/drain-stats)
#   OUT           output dir (default: <TARGET_DIR>/repro)
#   CASES         page paths relative to the repo root, without `.html`
#   RUN_SECONDS   per-case run length before SIGKILL (default: 30)
#   SKIP_BUILD=1  use the existing servoshell instead of rebuilding
#
# Dromaeo pages are opened directly, so htmlrunner.js runs each test once and
# the process never exits; the timeout is the stop condition.
# tests/gc pages exit on their own (they call window.close()); the timeout is
# only a safety net. Each drain is written immediately, so a SIGKILLed process
# keeps the records written before the kill.
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/../../.." && pwd)
if [ -z "${QJS_DIR:-}" ]; then
  if [ -d "$REPO_ROOT/servo-quickjs" ]; then
    QJS_DIR=$REPO_ROOT/servo-quickjs
  else
    QJS_DIR=$(cd "$REPO_ROOT/.." && pwd)/servo-quickjs
  fi
fi
TARGET_DIR=${TARGET_DIR:-$REPO_ROOT/target/drain-stats}
OUT=${OUT:-$TARGET_DIR/repro}
# Other cases kept for reference:
# CASES=${CASES:-"
#   tests/dromaeo/dromaeo/web/tests/dom-attr
#   tests/dromaeo/dromaeo/web/tests/dom-query
#   tests/dromaeo/dromaeo/web/tests/dom-traverse
#   tests/dromaeo/dromaeo/web/tests/jslib-modify-jquery
#   tests/dromaeo/dromaeo/web/tests/jslib-modify-prototype
#   tests/gc/14-many-dom-clones-loop
#   tests/gc/15-many-dom-clones-variadic
#   tests/gc/16-scroll-remove-drain
# "}
CASES=${CASES:-"
  tests/gc/17-batch-remove-drain
  tests/gc/18-remove-subtree-drain-and-gc
"}
RUN_SECONDS=${RUN_SECONDS:-30}
SKIP_BUILD=${SKIP_BUILD:-0}

SERVOSHELL=$TARGET_DIR/debug/servoshell
MERGER=$SCRIPT_DIR/merge_drain_stats.py
PLOT_DRAIN=$SCRIPT_DIR/plot_drain_vs_values.py
PLOT_GC=$SCRIPT_DIR/plot_gc_vs_objects.py
PYTHON=${PYTHON:-$REPO_ROOT/.venv/bin/python}
[ -x "$PYTHON" ] || PYTHON=python3

# Run a plot script in an environment that has matplotlib: `uv run` supplies it
# on demand and `--no-project` keeps it away from the repo's uv.lock.
plot() {
  if command -v uv >/dev/null 2>&1; then
    uv run --quiet --no-project --with matplotlib --with numpy python "$@"
  else
    "$PYTHON" "$@"
  fi
}

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
  gc_stats=$OUT/qjs-gc-$page.jsonl
  rm -f "$stats" "$gc_stats"

  echo "running $page for ${RUN_SECONDS}s"
  # The `Killed` notice from the shell is noise here: the SIGKILL is the stop condition.
  status=0
  wall_file=$OUT/wall-$page.txt
  rm -f "$wall_file"
  ( /usr/bin/time -q -f '%e' -o "$wall_file" \
      timeout -s KILL "$RUN_SECONDS" env QJS_GC_STATS=1 \
      QJS_GC_STATS_OUT="$gc_stats" QJS_DRAIN_STATS_OUT="$stats" \
      "$SERVOSHELL" "$html" --headless > "$OUT/run-$page.log" 2>&1 &
    wait $! ) 2>/dev/null || status=$?
  case $status in
    0|124|137) ;;
    *) echo "  warning: servoshell exited with $status before the timeout" >&2 ;;
  esac
  if [ ! -s "$stats" ]; then
    echo "  warning: no drain record written (the page ran no JS, or died too early)" >&2
  fi
  if [ ! -s "$gc_stats" ]; then
    echo "  warning: no GC record written (the page triggered no JS_GC)" >&2
  fi

  if [ -s "$stats" ]; then
    plot "$PLOT_DRAIN" "$stats" -o "$OUT/drain-vs-values-$page.png"
  fi
  if [ -s "$gc_stats" ]; then
    plot "$PLOT_GC" "$gc_stats" -o "$OUT/gc-vs-objects-$page.png"
  fi
done

if [ -f "$MERGER" ]; then
  "$PYTHON" "$MERGER" "$OUT" --wall
fi

echo "outputs: $OUT"
echo "compare with etc/ci/performance/pool-gc-findings.md"
