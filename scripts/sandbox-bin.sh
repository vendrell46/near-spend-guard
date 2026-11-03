#!/bin/sh
# Prints the path of nearcore's sandbox binary, downloading it once.
#
# near-workspaces' build script downloads the same file but checks nothing
# beyond HTTPS, and nearcore publishes no checksum for it. This script pins
# the SHA-256 of the archive we first downloaded (3 Nov 2026), so a changed
# file stops the tests instead of running. Pass the path to cargo as
# NEAR_SANDBOX_BIN_PATH and near-workspaces skips its own download.
set -eu

VERSION=2.14.0
case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) PLATFORM=Darwin-arm64
        SHA256=ea53448ef97540c012e5c3d967c74d78292b0465a98cf7027811a936718fe232 ;;
    Linux-x86_64) PLATFORM=Linux-x86_64
        SHA256=a0ba004218b218bd74351795e7ce2eda4a2b6af5cd3e252bd5e8bfe3c8f2e691 ;;
    *) echo "no pinned nearcore sandbox binary for $(uname -s)-$(uname -m)" >&2
        exit 1 ;;
esac

ROOT=$(cd "$(dirname "$0")/.." && pwd)
DIR="$ROOT/target/near-sandbox/$VERSION"
BIN="$DIR/near-sandbox"

if [ ! -x "$BIN" ]; then
    mkdir -p "$DIR"
    # A private work directory, renamed into place only when complete, so an
    # interrupted run never leaves a half-written binary behind.
    WORK=$(mktemp -d "$DIR/download.XXXXXX")
    trap 'rm -rf "$WORK"' EXIT
    curl -fsSL -o "$WORK/near-sandbox.tar.gz" \
        "https://s3-us-west-1.amazonaws.com/build.nearprotocol.com/nearcore/$PLATFORM/$VERSION/near-sandbox.tar.gz"
    if command -v sha256sum >/dev/null 2>&1; then
        GOT=$(sha256sum "$WORK/near-sandbox.tar.gz" | cut -d ' ' -f 1)
    else
        GOT=$(shasum -a 256 "$WORK/near-sandbox.tar.gz" | cut -d ' ' -f 1)
    fi
    if [ "$GOT" != "$SHA256" ]; then
        echo "near-sandbox $VERSION for $PLATFORM: SHA-256 $GOT, expected $SHA256" >&2
        exit 1
    fi
    tar -xzf "$WORK/near-sandbox.tar.gz" -C "$WORK" "$PLATFORM/near-sandbox"
    mv "$WORK/$PLATFORM/near-sandbox" "$BIN"
fi
echo "$BIN"
