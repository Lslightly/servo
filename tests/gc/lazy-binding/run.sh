#!/usr/bin/env bash
# Regression tests for lazy DOM bindings (JSDomStub -> JSObject materialization).
#
# Usage:
#   tests/gc/lazy-binding/run.sh [--binary PATH] [--filter STR] [--timeout SECS] [--verbose]
#
#   --binary PATH   servoshell built with js-quickjs (default: target/debug/servoshell)
#   --filter STR    only run pages whose file name contains STR (e.g. --filter 04)
#   --timeout SECS  per-page timeout in seconds (default: 30)
#   --verbose       print every page's console output, not just failing ones
#
# Each page sets its own expectations and prints a `[PASS]`/`[FAIL]` summary
# before calling `window.close()`. The `materialize_*` counters only exist when
# the process is started with SERVO_LAZY_BINDING_STATS=1, which this runner sets.
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/../../.." && pwd)

BINARY=$REPO_ROOT/target/debug/servoshell
FILTER=""
TIMEOUT_SECS=30
VERBOSE=0

usage() {
  sed -n '2,16p' "$0" | sed -e 's/^# \{0,1\}//' -e '/^$/d'
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --binary) BINARY=${2:?--binary needs a path}; shift 2 ;;
    --binary=*) BINARY=${1#*=}; shift ;;
    --filter) FILTER=${2:?--filter needs a string}; shift 2 ;;
    --filter=*) FILTER=${1#*=}; shift ;;
    --timeout) TIMEOUT_SECS=${2:?--timeout needs seconds}; shift 2 ;;
    --timeout=*) TIMEOUT_SECS=${1#*=}; shift ;;
    --verbose|-v) VERBOSE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

# A relative --binary is resolved from the repository root, so the documented
# `--binary target/debug/servoshell` works from any directory.
[[ "$BINARY" = /* ]] || BINARY="$REPO_ROOT/$BINARY"
if [[ ! -x "$BINARY" ]]; then
  echo "error: $BINARY is not executable" >&2
  echo "build it with:" >&2
  echo "  ./mach build --features js-quickjs --config patch.crates-io.mozjs.path=\"servo-quickjs/mozjs\"" >&2
  exit 2
fi
if ! command -v timeout >/dev/null 2>&1; then
  echo "error: 'timeout' (coreutils) is required" >&2
  exit 2
fi

pages=("$SCRIPT_DIR"/*.html)
if [[ ! -e "${pages[0]}" ]]; then
  echo "error: no test pages in $SCRIPT_DIR" >&2
  exit 2
fi

tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT

total=0
failed=0
skipped=0

for page in "${pages[@]}"; do
  name=$(basename "$page")
  if [[ -n "$FILTER" && "$name" != *"$FILTER"* ]]; then
    skipped=$((skipped + 1))
    continue
  fi
  total=$((total + 1))

  log=$tmpdir/${name%.html}.log
  status=0
  # The page exits by itself (`window.close()`); `--exit` and the timeout are
  # safety nets for a page that throws before it can close the window.
  timeout --kill-after=5 "${TIMEOUT_SECS}s" \
    env SERVO_LAZY_BINDING_STATS=1 "$BINARY" -z --exit "file://$page" \
    >"$log" 2>&1 || status=$?

  failures=$(grep -c '^\[FAIL\] ' "$log" 2>/dev/null || true)
  # The page's final summary is `[PASS] <page> (...)`; a page that dies early
  # prints no summary at all and must not pass just because it printed no
  # individual `[FAIL]` line.
  summary=$(grep -c "^\[PASS\] ${name%.html} " "$log" 2>/dev/null || true)

  if [[ $status -eq 124 || $status -eq 137 ]]; then
    echo "[FAIL] $name (timed out after ${TIMEOUT_SECS}s)"
    failed=$((failed + 1))
  elif [[ $failures -eq 0 && $summary -gt 0 ]]; then
    echo "[PASS] $name"
  else
    echo "[FAIL] $name (exit $status, $failures failed checks, $summary summaries)"
    failed=$((failed + 1))
  fi

  if [[ $VERBOSE -eq 1 || $failures -gt 0 || $summary -eq 0 ]]; then
    sed 's/^/    /' "$log"
  fi
done

passed=$((total - failed))
if [[ $skipped -gt 0 ]]; then
  echo "lazy-binding: $passed/$total pages passed ($skipped skipped by --filter)"
else
  echo "lazy-binding: $passed/$total pages passed"
fi
[[ $failed -eq 0 ]]
