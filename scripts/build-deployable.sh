#!/bin/sh
# Builds the wasm files we deploy to testnet with cargo-near 0.22.0: the
# compile scripts/build-wasm.sh does, then wasm-opt, with the contract's ABI
# embedded. Always --locked; pass --offline to build from the local crate
# cache.
#
# cargo-near writes its intermediate wasm into the cargo target directory,
# built with other features than scripts/build-wasm.sh uses, so it gets its
# own (target/near-build) and leaves the sandbox tests' build alone. It has
# to be an absolute path: cargo-near runs cargo in the crate's directory. The
# files to deploy land in target/deploy, with target/deploy/SHA256SUMS. The
# naive guard is never deployed; it's built the same way so the demo tests
# compare like with like. scripts/test-deployable.sh runs the sandbox tests
# against these files.
set -eu
cd "$(dirname "$0")/.."
for arg in "$@"; do
    case "$arg" in
        --offline) CARGO_NET_OFFLINE=true; export CARGO_NET_OFFLINE ;;
        *) echo "usage: $0 [--offline]" >&2; exit 2 ;;
    esac
done
VERSION=$(cargo near --version 2>/dev/null) || VERSION="none"
if [ "$VERSION" != "cargo-near-near 0.22.0" ]; then
    echo "needs cargo-near 0.22.0 on PATH (found: $VERSION)" >&2
    exit 1
fi

OUT=target/deploy
rm -rf "$OUT"
for crate in guard test-token naive-guard; do
    CARGO_TARGET_DIR="$PWD/target/near-build" cargo near build non-reproducible-wasm \
        --locked --color never --manifest-path "contracts/$crate/Cargo.toml" --out-dir "$OUT"
done
cd "$OUT"
if command -v sha256sum >/dev/null 2>&1; then
    sha256sum ./*.wasm >SHA256SUMS
else
    shasum -a 256 ./*.wasm >SHA256SUMS
fi
wc -c ./*.wasm
cat SHA256SUMS
