#!/bin/sh
# Unit and sandbox tests in a container, for systems scripts/sandbox-bin.sh
# has no binary for (an Intel Mac, Windows, Linux on arm). Runs this checkout;
# build output stays in a named volume, so the host's target/ isn't touched.
# The image is pinned by digest; everything else it fetches comes through
# rust-toolchain.toml, Cargo.lock and scripts/sandbox-bin.sh.
set -eu
cd "$(dirname "$0")/.."
IMAGE=rust:1.97.0@sha256:b92b8c8574f8f3b207fcb0912fb3e2de4041580b5934d90312d53938c9a038a9
exec docker run --rm --platform linux/amd64 \
    -v "$PWD":/w -v near-spend-guard-target:/w/target -w /w "$IMAGE" \
    sh -c 'cargo test --workspace --exclude sandbox-tests --locked && scripts/test-sandbox.sh --locked'
