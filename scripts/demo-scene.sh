#!/bin/sh
# Each scene of the demo video, as the command that produces it (DEMO.md).
#
#   scripts/demo-scene.sh 1 [cargo flags]   ten payments at once: ours exactly 100, the naive guard more
#   scripts/demo-scene.sh 2 [cargo flags]   the price changes after approval: the naive guard pays 190, ours refuses
#   scripts/demo-scene.sh 3 [<dir>]         an agent pays for an API: paid, refused, pending
#   scripts/demo-scene.sh tidy [<dir>]      before recording: prune expired requests, list what's pending
#
# Scenes 1 and 2 are sandbox tests that print their verdict lines and assert
# them (sandbox-tests/tests/demo.rs); pass --offline --locked after a first
# fetch. Scene 3 speaks MCP to the `pay` server as a host does, against the
# world in <dir>: testnet by default (scripts/deploy-testnet.sh's
# ~/.config/near-spend-guard/testnet), or a directory scripts/local-node.sh is
# serving. For the video, Claude Code takes the host's place:
# scripts/local-agent.sh <dir> "<prompt>".
set -eu
if [ $# -lt 1 ]; then
    echo "usage: scripts/demo-scene.sh 1|2|3|tidy [...]" >&2
    exit 2
fi
SCENE=$1
shift
cd "$(dirname "$0")/.."
case "$SCENE" in
    1) exec scripts/test-sandbox.sh "$@" --test demo race -- --nocapture ;;
    2) exec scripts/test-sandbox.sh "$@" --test demo price_swap -- --nocapture ;;
    3)
        DIR=${1:-$HOME/.config/near-spend-guard/testnet}
        if [ ! -f "$DIR/world.json" ] || [ ! -f "$DIR/agent.env" ]; then
            echo "$DIR has no demo world: run scripts/deploy-testnet.sh <prefix> (or scripts/local-node.sh $DIR)" >&2
            exit 1
        fi
        NODE=$(command -v node) || { echo "needs node on PATH" >&2; exit 1; }
        exec env -i PATH="$(dirname "$NODE"):/usr/bin:/bin" HOME="$HOME" node client/deploy/scene3.ts "$DIR"
        ;;
    tidy)
        DIR=${1:-$HOME/.config/near-spend-guard/testnet}
        NODE=$(command -v node) || { echo "needs node on PATH" >&2; exit 1; }
        exec env -i PATH="$(dirname "$NODE"):/usr/bin:/bin" HOME="$HOME" node client/deploy/tidy.ts "$DIR"
        ;;
    *)
        echo "usage: scripts/demo-scene.sh 1|2|3|tidy [...]" >&2
        exit 2
        ;;
esac
