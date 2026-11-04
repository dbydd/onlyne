#!/usr/bin/env bash
# Starts the Onlyne MCP server of the cluster experiment (stdio MCP + HTTP node).
#
# An MCP client should spawn this process itself. Running it in a terminal is for a smoke
# check that the node binds; the stdio session then waits on stdin.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"

cd "$root"
exec bun packages/mcp-server/src/main.ts examples/cluster/gateways/mcp-server.json
