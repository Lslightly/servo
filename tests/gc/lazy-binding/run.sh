#!/usr/bin/env bash

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

# Compatibility entry point for all assertion-based lazy-binding suites.
#
# Prefer functional/run.sh for normal materialization regressions,
# runtime/run.sh when testing the window.runtime helper itself, and
# memory/run.sh for JSObject/DOM-stub/autorelease-pool memory analysis.
#
# Usage:
#   run.sh [--binary PATH] [--filter STR] [--timeout SECS] [--verbose]

set -o errexit
set -o nounset
set -o pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

usage() {
  sed -n '7,13p' "$0" | sed -e 's/^# \{0,1\}//' | sed '/./!d'
}

if [[ ${1:-} == -h || ${1:-} == --help ]]; then
  usage
  exit 0
fi

status=0
"${SCRIPT_DIR}/functional/run.sh" "$@" || status=$?
"${SCRIPT_DIR}/runtime/run.sh" "$@" || status=$?
"${SCRIPT_DIR}/memory/run.sh" "$@" || status=$?
exit "${status}"
