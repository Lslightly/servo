#!/usr/bin/env bash
# Run all assertion-based DOM lazy-binding regression suites.

set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "$SCRIPT_DIR/lib.sh"

usage() {
  cat <<'EOF'
usage: run-functional.sh [--binary PATH] [--filter STR] [--timeout SECS] [--verbose]

Run tests/gc/lazy-binding functional and runtime checks with the release
QuickJS servoshell by default.
EOF
}

binary=
runner_args=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --binary) binary=${2:?--binary needs a path}; shift 2 ;;
    --binary=*) binary=${1#*=}; shift ;;
    --filter) runner_args+=(--filter "${2:?--filter needs a string}"); shift 2 ;;
    --filter=*) runner_args+=("--filter=${1#*=}"); shift ;;
    --timeout) runner_args+=(--timeout "${2:?--timeout needs seconds}"); shift 2 ;;
    --timeout=*) runner_args+=("--timeout=${1#*=}"); shift ;;
    --verbose|-v) runner_args+=(--verbose); shift ;;
    -h|--help) usage; exit 0 ;;
    *) lt_usage_error "unknown argument: $1" ;;
  esac
done

binary=$(lt_binary "$binary")
results=$(lt_results_dir functional)
log=$results/functional.log

echo "functional: $binary"
echo "results: $results"

set -o pipefail
"$REPO_ROOT/tests/gc/lazy-binding/run.sh" \
  --binary "$binary" "${runner_args[@]}" 2>&1 | tee "$log"

