#!/bin/sh
# Claude Code as the agent, with one tool: `pay`, served by nsg-pay-mcp against
# the local node that scripts/local-node.sh started in <dir>. Usage:
#
#   scripts/local-agent.sh <dir> "<prompt>" [more claude flags]
#
# The host gets no built-in tools (no shell, no files, no web) and no MCP
# server but ours, and `pay` is the only tool it may call without asking. The
# agent's key stays in <dir>/agent.env, read by the server through
# `node --env-file`; it is never in this script's environment or the host's.
# Prints the host's stream-json events (each tool call and its answer).
set -eu
if [ $# -lt 2 ]; then
    echo "usage: scripts/local-agent.sh <dir> \"<prompt>\" [claude flags]" >&2
    exit 2
fi
DIR=$1
PROMPT=$2
shift 2
if [ ! -f "$DIR/mcp.json" ] || [ ! -f "$DIR/agent.env" ]; then
    echo "$DIR has no local node: run scripts/local-node.sh $DIR first" >&2
    exit 1
fi
# Away from this repo, so no project settings or CLAUDE.md come along.
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cd "$WORK"
claude -p "$PROMPT" \
    --mcp-config "$DIR/mcp.json" --strict-mcp-config \
    --tools "" \
    --allowedTools "mcp__near-spend-guard__pay" \
    --permission-mode default \
    --setting-sources local \
    --no-session-persistence \
    --output-format stream-json --verbose \
    "$@"
