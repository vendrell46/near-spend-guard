#!/bin/sh
# Builds the contracts the sandbox tests deploy, one crate at a time so that
# the token's dependencies can't change the guard's features.
#
# This is the compile step `cargo near build` runs before wasm-opt: `--cfg
# near` selects near-sdk's on-chain host functions. Extra arguments go to
# cargo (for example --offline --locked).
set -eu
cd "$(dirname "$0")/.."
for crate in near-spend-guard naive-guard test-token; do
    RUSTFLAGS="--cfg near -C link-arg=-s" \
        cargo build -p "$crate" --target wasm32-unknown-unknown --release "$@"
done
