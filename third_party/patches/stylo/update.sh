#!/usr/bin/env bash
# Vendors Stylo into third_party/stylo/: upstream servo/stylo at the commit Cargo.toml pins
# (the `rev` of the `stylo` git dependency) with the patches in this directory applied.
#
#   ./update.sh                                Replace third_party/stylo/ and stage it.
#   ./update.sh --check                        Compare third_party/stylo/ with a fresh
#                                              vendoring; exit non-zero if they differ.
#   ./update.sh --export-patches <repo> <rev>  Replace the patches here with the commits from
#                                              the pinned upstream commit to <rev> in the Stylo
#                                              checkout <repo>.
#
# Cargo.toml keeps the upstream pin in the `git = ..., rev = ...` lines exactly as upstream Servo
# writes them and redirects the crates to third_party/stylo/ with a `[patch]` table, so an
# upstream bump of the pin only needs a re-run of this script.
set -euo pipefail

UPSTREAM=https://github.com/servo/stylo.git

cd "$(dirname "$0")"
repo_root=$(git rev-parse --show-toplevel)
vendored_dir=$repo_root/third_party/stylo

usage() {
    sed -n '2,11s/^# \{0,1\}//p' "$0" >&2
    exit 2
}

upstream_commit=$(sed -n 's/^stylo = { git = "https:\/\/github.com\/servo\/stylo", rev = "\([0-9a-f]\{40\}\)" }$/\1/p' "$repo_root/Cargo.toml")
if [ -z "$upstream_commit" ]; then
    echo "Could not find the stylo git pin in $repo_root/Cargo.toml" >&2
    exit 1
fi

if [ "${1-}" = --export-patches ]; then
    [ $# -eq 3 ] || usage
    repo=$2
    rev=$3
    rm -f ./*.patch
    git -C "$repo" format-patch --quiet --zero-commit --no-signature -o "$PWD" "$upstream_commit..$rev"
    echo "Exported $(ls ./*.patch 2>/dev/null | wc -l) patches to $PWD."
    exit 0
fi

check=false
if [ "${1-}" = --check ]; then
    check=true
    shift
fi
[ $# -eq 0 ] || usage

work=$(mktemp -d)
trap 'rm -rf "${work:?}"' EXIT

crate="$work/stylo"
git init -q "$crate"
git -C "$crate" fetch -q --depth 1 "$UPSTREAM" "$upstream_commit"
git -C "$crate" checkout -q FETCH_HEAD
if ls ./*.patch > /dev/null 2>&1; then
    git -C "$crate" -c user.name=update.sh -c user.email=update.sh@invalid am -q --whitespace=nowarn "$PWD"/*.patch
fi

# Everything upstream tracks except its CI and git configuration.
vendored="$work/vendored"
git -C "$crate" ls-files -z | grep -zv '^\.github/\|^\.gitignore$' |
    (cd "$crate" && cpio --quiet -0 -pdm "$vendored")

if $check; then
    if git diff --no-index --quiet "$vendored_dir" "$vendored"; then
        echo "third_party/stylo/ is up to date."
        exit 0
    fi
    { git diff --no-index --stat "$vendored_dir" "$vendored" || true; } | sed "s#$vendored#(fresh)#"
    echo "third_party/stylo/ differs from a fresh vendoring."
    exit 1
fi

rm -rf "$vendored_dir"
mv "$vendored" "$vendored_dir"
git -C "$repo_root" add --all --force third_party/stylo
echo "Vendored and staged Stylo in third_party/stylo/."
