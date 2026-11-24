#!/usr/bin/env bash
# Measure hot DOM binding paths and optionally compare a baseline binary.

set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "$SCRIPT_DIR/lib.sh"

usage() {
  cat <<'EOF'
usage: run-dom-microbenchmark.sh [options]

Options:
  --binary PATH                 candidate servoshell (default: target/release/servoshell)
  --baseline-binary PATH        optional baseline servoshell
  --runs N                      process runs per binary (default: 5)
  --timeout SECS                per-run timeout (default: 120)
  --max-regression-percent N    fail when candidate exceeds baseline by more than N%
  -h, --help                    show this help

Results are written under .vscode/logs/local-tests/<timestamp>-dom-microbenchmark/.
Without --max-regression-percent the command always reports and exits successfully.
EOF
}

binary=
baseline=
runs=5
timeout_secs=120
max_regression=
while [[ $# -gt 0 ]]; do
  case "$1" in
    --binary) binary=${2:?--binary needs a path}; shift 2 ;;
    --binary=*) binary=${1#*=}; shift ;;
    --baseline-binary) baseline=${2:?--baseline-binary needs a path}; shift 2 ;;
    --baseline-binary=*) baseline=${1#*=}; shift ;;
    --runs) runs=${2:?--runs needs a number}; shift 2 ;;
    --runs=*) runs=${1#*=}; shift ;;
    --timeout) timeout_secs=${2:?--timeout needs seconds}; shift 2 ;;
    --timeout=*) timeout_secs=${1#*=}; shift ;;
    --max-regression-percent)
      max_regression=${2:?--max-regression-percent needs a number}
      shift 2
      ;;
    --max-regression-percent=*) max_regression=${1#*=}; shift ;;
    -h|--help) usage; exit 0 ;;
    *) lt_usage_error "unknown argument: $1" ;;
  esac
done

[[ $runs =~ ^[1-9][0-9]*$ ]] || lt_usage_error "--runs must be a positive integer"
if [[ -n $max_regression && ! $max_regression =~ ^[0-9]+([.][0-9]+)?$ ]]; then
  lt_usage_error "--max-regression-percent must be non-negative"
fi

binary=$(lt_binary "$binary")
if [[ -n $baseline ]]; then
  baseline=$(lt_binary "$baseline")
fi

page=$REPO_ROOT/tests/html/dom-microbenchmark.html
results=$(lt_results_dir dom-microbenchmark)
bench_order=(
  "el.id"
  "document.body"
  "el.parentNode"
  "el.setAttribute"
  "document.getElementById"
)

median_stream() {
  sort -n | awk '
    { values[NR] = $1 }
    END {
      if (NR == 0) exit 1
      if (NR % 2 == 1) {
        printf "%.3f\n", values[(NR + 1) / 2]
      } else {
        printf "%.3f\n", (values[NR / 2] + values[NR / 2 + 1]) / 2
      }
    }
  '
}

run_binary() {
  local label=$1
  local executable=$2
  local raw_dir=$results/$label
  local summary=$results/$label.tsv
  local name run log status
  local -A samples=()
  local -A iterations=()

  mkdir -p "$raw_dir"
  for ((run = 1; run <= runs; run++)); do
    log=$raw_dir/run-$run.log
    echo "$label run $run/$runs"
    status=0
    lt_run_page "$executable" "$page" "$log" "$timeout_secs" || status=$?
    if [[ $status -ne 0 ]]; then
      sed 's/^/    /' "$log" >&2 || true
      lt_usage_error "$label run $run failed with exit $status"
    fi
    while read -r name count elapsed; do
      samples[$name]+="$elapsed"$'\n'
      iterations[$name]=$count
    done < <(awk '/^\[BENCH\] / { print $2, $3, $4 }' "$log")
  done

  printf 'case\titerations\tmedian_ms\n' >"$summary"
  for name in "${bench_order[@]}"; do
    [[ -n ${samples[$name]:-} ]] || lt_usage_error "$label did not report benchmark: $name"
    local median
    median=$(printf '%s' "${samples[$name]}" | median_stream)
    printf '%s\t%s\t%s\n' "$name" "${iterations[$name]}" "$median" >>"$summary"
  done
}

run_binary candidate "$binary"
if [[ -n $baseline ]]; then
  run_binary baseline "$baseline"
fi

echo
if [[ -n $baseline ]]; then
  echo "case	baseline_ms	candidate_ms	change_percent"
else
  echo "case	candidate_ms"
fi

failed=0
for name in "${bench_order[@]}"; do
  candidate=$(awk -F '\t' -v name="$name" '$1 == name { print $3 }' "$results/candidate.tsv")
  if [[ -z $baseline ]]; then
    printf '%s\t%s\n' "$name" "$candidate"
    continue
  fi

  baseline_ms=$(awk -F '\t' -v name="$name" '$1 == name { print $3 }' "$results/baseline.tsv")
  read -r change failed_now < <(
    awk -v base="$baseline_ms" -v candidate="$candidate" -v max="$max_regression" '
      BEGIN {
        if (base <= 0) {
          print "n/a 1"
          exit
        }
        change = (candidate / base - 1) * 100
        failed = (max != "" && change > max) ? 1 : 0
        printf "%.2f %d\n", change, failed
      }
    '
  )
  printf '%s\t%s\t%s\t%s%%\n' "$name" "$baseline_ms" "$candidate" "$change"
  failed=$((failed + failed_now))
done

echo
echo "raw results: $results"
if [[ -n $max_regression && $failed -ne 0 ]]; then
  echo "regression threshold exceeded: ${max_regression}%" >&2
  exit 1
fi

