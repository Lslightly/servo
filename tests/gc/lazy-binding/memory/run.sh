#!/usr/bin/env bash

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

# Memory analysis for lazy binding: each page reads the engine's JSObject and
# JSDomStub counters through `window.runtime.memory_usage()` (one heap walk
# returning [objects, objectBytes, stubs, stubBytes]), asserts the count
# relationships it documents, and prints `[PASS]`/`[FAIL]` lines. This runner
# only launches the pages and reports their summaries.
#
# Usage:
#   run.sh [--binary PATH] [--filter STR] [--timeout SECS] [--verbose]

set -o errexit
set -o nounset
set -o pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "${SCRIPT_DIR}/../../../.." && pwd)
PREF_FILE="${REPO_ROOT}/resources/wpt-prefs.json"

BINARY="${REPO_ROOT}/target/release/servoshell"
FILTER=""
TIMEOUT_SECS=60
VERBOSE=0

usage() {
  sed -n '7,16p' "$0" | sed -e 's/^# \{0,1\}//' | sed '/./!d'
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

[[ "${BINARY}" = /* ]] || BINARY="${REPO_ROOT}/${BINARY}"
if [[ ! -x "${BINARY}" ]]; then
  echo "error: ${BINARY} is not executable" >&2
  exit 2
fi
if [[ ! -f "${PREF_FILE}" ]]; then
  echo "error: ${PREF_FILE} is missing" >&2
  exit 2
fi

pages=("${SCRIPT_DIR}"/*.html)
tmpdir=$(mktemp -d)
trap 'rm -rf "${tmpdir}"' EXIT

total=0
failed=0
skipped=0

for page in "${pages[@]}"; do
  name=$(basename "${page}")
  base=${name%.html}
  if [[ -n "${FILTER}" && "${name}" != *"${FILTER}"* ]]; then
    skipped=$((skipped + 1))
    continue
  fi
  total=$((total + 1))

  log=${tmpdir}/${base}.log
  status=0
  timeout --kill-after=5 "${TIMEOUT_SECS}s" \
    env SERVO_LAZY_BINDING_STATS=1 \
    "${BINARY}" -z \
    --prefs-file "${PREF_FILE}" "file://${page}" \
    >"${log}" 2>&1 || status=$?

  failures=$(grep -c '^\[FAIL\] ' "${log}" 2>/dev/null || true)
  summary=$(grep -c "^\[PASS\] ${base} " "${log}" 2>/dev/null || true)

  page_failed=0
  if [[ ${status} -eq 124 || ${status} -eq 137 ]]; then
    echo "[FAIL] ${name} (timed out after ${TIMEOUT_SECS}s)"
    page_failed=1
  elif [[ ${failures} -eq 0 && ${summary} -gt 0 ]]; then
    echo "[PASS] ${name}"
  else
    echo "[FAIL] ${name} (exit ${status}, ${failures} failed checks, ${summary} summaries)"
    page_failed=1
  fi

  if [[ ${page_failed} -ne 0 ]]; then
    failed=$((failed + 1))
  fi

  if [[ ${VERBOSE} -eq 1 || ${page_failed} -ne 0 ]]; then
    sed 's/^/    /' "${log}"
  fi
done

passed=$((total - failed))
if [[ ${skipped} -gt 0 ]]; then
  echo "lazy-binding memory: ${passed}/${total} pages passed (${skipped} skipped by --filter)"
else
  echo "lazy-binding memory: ${passed}/${total} pages passed"
fi
[[ ${failed} -eq 0 ]]
