#!/bin/bash
# Usage: rss-rounds.sh <servoshell> <url of rss-rounds.html>
# Prints the PSS and its anon/file/shmem breakdown of the process once the page logs DONE.
# PSS charges shared pages proportionally to the processes that map them, so runs can be
# compared without RSS inflating on shared mappings; it has no peak/high-water mark.

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(git -C "$script_dir" rev-parse --show-toplevel 2>/dev/null) ||
    repo_root=$(cd -- "$script_dir/../../../.." && pwd)
log_dir="$repo_root/.vscode/logs/rss-rounds/$(date +%Y%m%d-%H%M%S)"
mkdir -p "$log_dir"
log="$log_dir/rss-rounds.log"
"$1" --headless --pref dom_testutils_enabled=true "$2" > "$log" 2>&1 &
pid=$!
while kill -0 "$pid" 2>/dev/null && ! grep -q '^DONE' "$log"; do sleep 0.2; done
grep -E '^(Pss|Pss_Anon|Pss_File|Pss_Shmem):' "/proc/$pid/smaps_rollup" | tr -s ' \t' ' '
kill "$pid"