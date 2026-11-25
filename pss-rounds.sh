#!/bin/bash
# Usage: pss-rounds.sh <servoshell> <url> [label] [out_dir]
#    or: PSS_OUT_DIR=<dir> pss-rounds.sh <servoshell> <url> [label]
#
# Runs <url> (normally pss-rounds.html) headless and samples /proc/<pid>/smaps_rollup once per
# round, whenever the page logs `ROUND <n>` and then idles (see pss-rounds.html). `ROUND 0` is
# the baseline sample before any allocation. Results go to
#   <out_dir>/<label>.csv        (default out_dir: <repo>/.vscode/logs/pss-rounds/<timestamp>,
#                                 or $PWD/logs/pss-rounds/<timestamp> outside a git repo)
# with columns: round,pss_kb,pss_anon_kb,pss_file_kb,pss_shmem_kb,rss_kb
# This script only needs bash and procfs, so it can be copied into any repo; pass an explicit
# out_dir (or put both scripts on a shared path) when collecting runs from different checkouts.
# Compare two or more runs from anywhere with:
#   uv run --with pandas --with matplotlib python pss-compare.py a=a.csv b=b.csv
#
# PSS charges shared pages proportionally to the processes that map them, so runs can be
# compared without PSS inflating on shared mappings; it has no peak/high-water mark, so the
# sample point is what makes runs comparable. Page URL query params: n, rounds, settle.
# Only the servoshell process itself is sampled, not any child processes.

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)

if [ $# -lt 2 ]; then
    echo "usage: $0 <servoshell> <url> [label] [out_dir]" >&2
    exit 2
fi

bin=$1
url=$2
label=$(printf '%s' "${3:-run}" | tr -c 'A-Za-z0-9._-' '-')

out_dir=${4:-${PSS_OUT_DIR:-}}
if [ -z "$out_dir" ]; then
    root=$(git -C "$script_dir" rev-parse --show-toplevel 2>/dev/null) || root=$PWD
    out_dir="$root/.vscode/logs/pss-rounds/$(date +%Y%m%d-%H%M%S)"
fi
log_dir=$out_dir
mkdir -p "$log_dir"
log="$log_dir/$label.log"
csv="$log_dir/$label.csv"

printf 'round,pss_kb,pss_anon_kb,pss_file_kb,pss_shmem_kb,rss_kb\n' > "$csv"
{
    printf 'binary=%s\nurl=%s\ndate=%s\nhost=%s\n' "$bin" "$url" "$(date -Is)" "$(uname -srm)"
} > "$log_dir/$label.meta"

sample() {
    local round=$1 row
    row=$(awk -v r="$round" '
        /^Pss:/       { pss   = $2 }
        /^Pss_Anon:/  { anon  = $2 }
        /^Pss_File:/  { file  = $2 }
        /^Pss_Shmem:/ { shmem = $2 }
        /^Rss:/       { rss   = $2 }
        END { printf "%s,%s,%s,%s,%s,%s\n", r, pss, anon, file, shmem, rss }
    ' "/proc/$pid/smaps_rollup" 2>/dev/null)
    [ -n "$row" ] && printf '%s\n' "$row" >> "$csv"
}

"$bin" --headless --pref dom_testutils_enabled=true "$url" > "$log" 2>&1 &
pid=$!
trap 'kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null' EXIT

off=0
finished=0
while kill -0 "$pid" 2>/dev/null; do
    # Feed only new lines; `ROUND <n>` is followed by `settle` ms of idle time in the page,
    # so the /proc read below happens while nothing is being allocated.
    while IFS=$'\t' read -r nr line; do
        off=$nr
        case "$line" in
            "ROUND "*) sample "${line#ROUND }" ;;
            "DONE") finished=1 ;;
        esac
    done < <(awk -v s="$off" 'NR > s && /^(ROUND [0-9]+|DONE)$/ { print NR "\t" $0 }' "$log" 2>/dev/null)
    [ "$finished" = 1 ] && break
    sleep 0.02
done

printf 'samples: %s\n' "$csv"
awk -F, 'NR > 1 { if (n == 0) first = $2; last = $2; n++ }
         END { if (n) printf "rounds=%d  first=%.1f MiB  last=%.1f MiB\n", NR - 1, first / 1024, last / 1024 }' "$csv"
