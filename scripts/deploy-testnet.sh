#!/bin/sh
# Deploys the demo world to testnet (client/deploy/testnet.ts): four faucet
# accounts, the test token and its receivers, the guard with the demo's
# policy, and the agent's function-call key. Usage:
#
#   scripts/build-deployable.sh --offline && scripts/test-deployable.sh --offline --locked
#   scripts/deploy-testnet.sh [--connection <near CLI connection>] <prefix> [<dir>]
#
# It deploys the files in target/deploy, and only if they still match
# target/deploy/SHA256SUMS, so what goes on chain is what the sandbox tests
# ran on. Running it again finishes a deploy that stopped half-way and sends
# nothing once everything is in place. Node gets only PATH and HOME, so no
# API key in this shell reaches it or near-api-js. Testnet only.
set -eu
if [ $# -lt 1 ] || [ $# -gt 4 ]; then
    echo "usage: scripts/deploy-testnet.sh [--connection <name>] <prefix> [<dir>]" >&2
    exit 2
fi
cd "$(dirname "$0")/.."
if [ ! -f target/deploy/SHA256SUMS ]; then
    echo "no target/deploy/SHA256SUMS: run scripts/build-deployable.sh first" >&2
    exit 1
fi
if command -v sha256sum >/dev/null 2>&1; then
    (cd target/deploy && sha256sum -c --quiet SHA256SUMS)
else
    (cd target/deploy && shasum -a 256 -c --quiet SHA256SUMS)
fi
NODE=$(command -v node) || { echo "needs node on PATH" >&2; exit 1; }
exec env -i PATH="$(dirname "$NODE"):/usr/bin:/bin" HOME="$HOME" \
    node client/deploy/testnet.ts "$@"
