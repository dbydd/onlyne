#!/usr/bin/env bash
# Starts the Grok Bot communication node of the cluster experiment.
#
# The webhook URL and sender key are read from the environment. Copy `.env.example` to `.env`
# and fill GROK_BOT_WEBHOOK_URL / GROK_BOT_WEBHOOK_KEY; this script sources `.env` when present.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"

if [[ -f "$here/.env" ]]; then
  # shellcheck disable=SC1091
  source "$here/.env"
  echo "using $here/.env" >&2
fi

cd "$root"
exec bun packages/grok-bot/src/main.ts examples/cluster/gateways/grok-bot.json
