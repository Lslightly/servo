#!/usr/bin/env bash
# Vendors freetype-sys into third_party/freetype-sys/: upstream at UPSTREAM_COMMIT with the
# patches in this directory applied, and libpng LIBPNG_VERSION and FreeType FREETYPE_VERSION
# vendored from their own upstreams with the crate's update-libpng.sh and update-freetype.sh.
#
#   ./update.sh                                Replace third_party/freetype-sys/ and stage it.
#   ./update.sh --check                        Compare third_party/freetype-sys/ with a fresh
#                                              vendoring; exit non-zero if they differ.
#   ./update.sh --export-patches <repo> <rev>  Replace the patches here with the commits from
#                                              UPSTREAM_COMMIT to <rev> in the freetype-sys
#                                              checkout <repo>. Commits that only write the
#                                              vendored libpng and FreeType sources are left out,
#                                              since this script vendors those from upstream.
#
# Keep VENDORED in sync with the files build.rs compiles and the headers they include. A missing
# file shows up as a compile error of a `bundled` build, e.g. for OpenHarmony or Android.
set -euo pipefail

UPSTREAM=https://github.com/PistonDevelopers/freetype-sys.git
UPSTREAM_COMMIT=7283f67faaeeb3dca7befd84b9fa626d947a9236
LIBPNG_VERSION=1.6.58
FREETYPE_VERSION=2.14.3
# Only used while the crate takes FreeType from a submodule (see FREETYPE_SUBMODULE).
FREETYPE_UPSTREAM=https://gitlab.freedesktop.org/freetype/freetype.git

# Files and directories of the patched freetype-sys checkout to vendor.
VENDORED=(
    Cargo.toml
    build.rs
    LICENSE
    README.md
    src
    libpng
    freetype2
)

# Used instead of freetype2/ while the crate still takes FreeType from a submodule, whose
# checkout is the whole upstream repository rather than the subset build.rs needs.
FREETYPE_SUBMODULE=(
    freetype2/include
    freetype2/src
    freetype2/builds/unix/ftsystem.c
    freetype2/builds/windows/ftsystem.c
    freetype2/LICENSE.TXT
    freetype2/README
    freetype2/docs/FTL.TXT
    freetype2/docs/GPLv2.TXT
)

cd "$(dirname "$0")"
repo_root=$(git rev-parse --show-toplevel)
vendored_dir=$repo_root/third_party/freetype-sys

usage() {
    sed -n '2,15s/^# \{0,1\}//p' "$0" >&2
    exit 2
}

if [ "${1-}" = --export-patches ]; then
    [ $# -eq 3 ] || usage
    repo=$2
    rev=$3
    rm -f ./*.patch
    number=1
    for commit in $(git -C "$repo" rev-list --reverse --no-merges "$UPSTREAM_COMMIT..$rev"); do
        # libpng/ and freetype2/ hold vendored upstream sources, which this script vendors with
        # the crate's own updaters instead, so commits that only write them are left out.
        if ! git -C "$repo" diff-tree --no-commit-id --name-only -r "$commit" |
            grep -qvE '^(libpng/|freetype2(/|$)|\.gitmodules$)'; then
            echo "Leaving out $(git -C "$repo" log -1 --format='%h %s' "$commit") (only vendors sources)"
            continue
        fi
        git -C "$repo" format-patch --quiet --zero-commit --no-signature --start-number "$number" \
            -o "$PWD" -1 "$commit"
        number=$((number + 1))
    done
    echo "Exported $((number - 1)) patches to $PWD."
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

crate="$work/crate"
git init -q "$crate"
git -C "$crate" fetch -q --depth 1 "$UPSTREAM" "$UPSTREAM_COMMIT"
git -C "$crate" checkout -q FETCH_HEAD
git -C "$crate" -c user.name=update.sh -c user.email=update.sh@invalid am -q --whitespace=nowarn "$PWD"/*.patch
"$crate/update-libpng.sh" "$LIBPNG_VERSION" > /dev/null
if [ -x "$crate/update-freetype.sh" ]; then
    "$crate/update-freetype.sh" "$FREETYPE_VERSION" > /dev/null
else
    tag=VER-${FREETYPE_VERSION//./-}
    git init -q "$crate/freetype2"
    git -C "$crate/freetype2" fetch -q --depth 1 "$FREETYPE_UPSTREAM" "refs/tags/$tag"
    git -C "$crate/freetype2" checkout -q FETCH_HEAD
    VENDORED=("${VENDORED[@]/freetype2}" "${FREETYPE_SUBMODULE[@]}")
fi

vendored="$work/freetype-sys"
for path in "${VENDORED[@]}"; do
    [ -n "$path" ] || continue
    mkdir -p "$vendored/$(dirname "$path")"
    cp -R "$crate/$path" "$vendored/$path"
done

if $check; then
    if git diff --no-index --quiet "$vendored_dir" "$vendored"; then
        echo "third_party/freetype-sys/ is up to date."
        exit 0
    fi
    { git diff --no-index --stat "$vendored_dir" "$vendored" || true; } | sed "s#$vendored#(fresh)#"
    echo "third_party/freetype-sys/ differs from a fresh vendoring."
    exit 1
fi

rm -rf "$vendored_dir"
mv "$vendored" "$vendored_dir"
git -C "$repo_root" add --all --force third_party/freetype-sys
echo "Vendored and staged freetype-sys in third_party/freetype-sys/."
