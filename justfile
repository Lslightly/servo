# Builds that take more than a plain `./mach` command. `just --list` lists the recipes.

set shell := ["bash", "-euo", "pipefail", "-c"]

genui_min_target := "aarch64-unknown-linux-ohos"

# GenUI-min is the smallest servoshell for generated pages on OpenHarmony. It runs on QuickJS,
# stores site data with the OpenHarmony RDB backend, and `--no-default-features` leaves out
# WebGPU, WebGL, WebXR, SubtleCrypto (`crypto` keeps only its own methods), gamepad, brotli in
# CompressionStream, multiprocess, devtools and jemalloc. The
# `genui-min` Cargo profile is `production` at opt-level "z" and with panic=abort, so a panic ends
# the app instead of showing a crash page. The linker packs the relative relocations (RELR) and
# folds identical code.
#
# Build the GenUI-min servoshell and its HAP; arguments go to `./mach build`
genui-min *args:
    RUSTFLAGS="${RUSTFLAGS:-} -Clink-arg=-Wl,-z,pack-relative-relocs -Clink-arg=-Wl,--icf=all" \
    ./mach build --target {{genui_min_target}} --js-engine quickjs --profile genui-min \
        --no-default-features --features "bundled max_log_level ohos-rdb-backend servo/clipboard" \
        {{args}} \
        --config 'profile.genui-min.inherits="production"' \
        --config 'profile.genui-min.opt-level="z"' \
        --config 'profile.genui-min.panic="abort"'

# Print the size of the GenUI-min library after stripping, and of its HAP
genui-min-size:
    #!/usr/bin/env bash
    set -euo pipefail
    shopt -s nullglob
    size() { stat -c %s "$1" | awk -v name="$2" '{ printf "%s: %d bytes (%.2f MiB)\n", name, $1, $1 / 1048576 }'; }
    lib="${CARGO_TARGET_DIR:-target}/{{genui_min_target}}/genui-min/libservoshell.so"
    "${LLVM_STRIP:-llvm-strip}" -o "${lib%.so}.stripped.so" "$lib"
    size "${lib%.so}.stripped.so" "libservoshell.so, stripped"
    for hap in target/openharmony/{{genui_min_target}}/genui-min/entry/build/*/outputs/default/*.hap; do
        size "$hap" "$(basename "$hap")"
    done
