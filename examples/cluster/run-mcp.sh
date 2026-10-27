#!/usr/bin/env bash
# Starts the MCP gateway of the cluster experiment.
#
# The gateway is not packaged as a CLI yet, so this runs the package entry through bun.
# The configuration holds repo-root-relative paths, so the process starts from the root.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"

cd "$root"
exec bun packages/mcp-gateway/src/main.ts examples/cluster/gateways/mcp.json
