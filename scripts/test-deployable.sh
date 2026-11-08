#!/bin/sh
# The sandbox tests against the files we deploy (target/deploy, built by
# scripts/build-deployable.sh) instead of the plain build. Arguments go to
# `cargo test`; --offline also goes to the build. The end-to-end client
# tests need Node: add `--test client -- --include-ignored --test-threads 1`.
set -eu
cd "$(dirname "$0")/.."
BUILD_FLAGS=""
for arg in "$@"; do
    case "$arg" in
        --offline) BUILD_FLAGS="--offline" ;;
    esac
done
# Unquoted on purpose: zero or one flag.
# shellcheck disable=SC2086
scripts/build-deployable.sh $BUILD_FLAGS
NEAR_SANDBOX_BIN_PATH=$(scripts/sandbox-bin.sh)
NSG_WASM_DIR="$PWD/target/deploy"
export NEAR_SANDBOX_BIN_PATH NSG_WASM_DIR
cargo test -p sandbox-tests "$@"
