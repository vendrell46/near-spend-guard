#!/bin/sh
# A local NEAR node with the demo's accounts that stays up, for running a real
# MCP host against the `pay` server without testnet
# (sandbox-tests/examples/local_node.rs). Usage:
#
#   scripts/local-node.sh <empty dir> [--offline --locked]
#
# Then, in another terminal, scripts/local-agent.sh <same dir> "<prompt>".
# `touch <dir>/stop` ends it (or Ctrl-C), and it ends by itself after
# NSG_LOCAL_MINUTES (default 120).
set -eu
if [ $# -lt 1 ]; then
    echo "usage: scripts/local-node.sh <empty dir> [cargo flags]" >&2
    exit 2
fi
DIR=$1
shift
case "$DIR" in
    /*) ;;
    *) DIR="$PWD/$DIR" ;;
esac
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
NSG_LOCAL_DIR=$DIR exec cargo run -q -p sandbox-tests --example local_node "$@"
