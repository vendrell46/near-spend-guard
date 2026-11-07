#!/bin/sh
# The TypeScript client: its own tests, then the client against the real
# contracts on a local node (`sandbox-tests/tests/client.rs`). Needs Node as
# well as what scripts/test-sandbox.sh needs. Arguments go to cargo test, so
# `scripts/test-client.sh --offline --locked` works after a first fetch.
set -eu
cd "$(dirname "$0")/.."
(
    cd client
    npm ci --ignore-scripts --no-audit --no-fund
    npm test
    npm run test:near
)
scripts/test-sandbox.sh "$@" --test client -- --include-ignored --nocapture --test-threads 1
