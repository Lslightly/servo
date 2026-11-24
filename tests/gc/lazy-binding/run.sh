#!/usr/bin/env bash
# Compatibility entry point for all assertion-based lazy-binding suites.
#
# Prefer functional/run.sh for normal materialization regressions and
# runtime/run.sh when testing the window.runtime helper itself.
#
# Usage:
#   run.sh [--binary PATH] [--filter STR] [--timeout SECS] [--verbose]
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

usage() {
  sed -n '2,8p' "$0" | sed -e 's/^# \{0,1\}//' -e '/^$/d'
}

if [[ ${1:-} == -h || ${1:-} == --help ]]; then
  usage
  exit 0
fi

status=0
"$SCRIPT_DIR/functional/run.sh" "$@" || status=$?
"$SCRIPT_DIR/runtime/run.sh" "$@" || status=$?
exit "$status"
