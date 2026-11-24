#!/usr/bin/env bash
# Shared helpers for the local test scripts.

LOCAL_TEST_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$LOCAL_TEST_DIR/../.." && pwd)
LT_PREF_FILE=$REPO_ROOT/resources/wpt-prefs.json
LT_DEFAULT_BINARY=$REPO_ROOT/target/release/servoshell

lt_usage_error() {
  echo "error: $*" >&2
  exit 2
}

lt_resolve_path() {
  local path=$1
  if [[ $path = /* ]]; then
    printf '%s\n' "$path"
  else
    printf '%s\n' "$REPO_ROOT/$path"
  fi
}

lt_binary() {
  local binary
  binary=$(lt_resolve_path "${1:-$LT_DEFAULT_BINARY}")
  [[ -x $binary ]] || lt_usage_error \
    "servoshell is not executable: $binary
build it with:
  UV_CACHE_DIR=\"\$PWD/target/uv-cache\" CARGO_NET_OFFLINE=true SERVO_QUICKJS_DEBUGGER=1 ./mach build --release --features js-quickjs --config 'patch.crates-io.mozjs.path=\"servo-quickjs/mozjs\"'"
  printf '%s\n' "$binary"
}

lt_results_dir() {
  local suite=$1
  local stamp
  local dir
  stamp=$(date +%Y%m%d-%H%M%S)
  dir=$REPO_ROOT/.vscode/logs/local-tests/$stamp-$suite
  mkdir -p "$dir"
  printf '%s\n' "$dir"
}

lt_require_page() {
  local page=$1
  [[ -f $page ]] || lt_usage_error "page is missing: $page"
}

lt_require_prefs() {
  [[ -f $LT_PREF_FILE ]] || lt_usage_error "preferences file is missing: $LT_PREF_FILE"
}

# Run one page in headless servoshell. Arguments:
#   binary page log timeout-seconds [QJS_GC_STATS]
lt_run_page() {
  local binary=$1
  local page=$2
  local log=$3
  local timeout_secs=$4
  local qjs_gc_stats=${5:-}
  local env_cmd=(env SERVO_LAZY_BINDING_STATS=1)

  lt_require_page "$page"
  lt_require_prefs
  if [[ -n $qjs_gc_stats ]]; then
    env_cmd+=(QJS_GC_STATS="$qjs_gc_stats")
  fi

  local status=0
  timeout --kill-after=5 "${timeout_secs}s" \
    "${env_cmd[@]}" "$binary" -z --exit \
    --prefs-file "$LT_PREF_FILE" "file://$page" \
    >"$log" 2>&1 || status=$?
  return "$status"
}

