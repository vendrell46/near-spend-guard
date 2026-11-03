#!/bin/sh
# Builds the contracts, fetches the pinned sandbox binary and runs the
# sandbox tests. Arguments go to `cargo test`; --offline, --locked and
# --frozen also go to the contract build, so
# `scripts/test-sandbox.sh --offline --locked` works after a first fetch.
set -eu
cd "$(dirname "$0")/.."
BUILD_FLAGS=""
for arg in "$@"; do
    case "$arg" in
        --offline | --locked | --frozen) BUILD_FLAGS="$BUILD_FLAGS $arg" ;;
    esac
done
# Unquoted on purpose: zero or more flags.
# shellcheck disable=SC2086
scripts/build-wasm.sh $BUILD_FLAGS
NEAR_SANDBOX_BIN_PATH=$(scripts/sandbox-bin.sh)
export NEAR_SANDBOX_BIN_PATH
cargo test -p sandbox-tests "$@"
