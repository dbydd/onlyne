#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
ONLYNE_CHANNEL=telegram ONLYNE_ENV_PREFIX=TELEGRAM ../shared/channel-smoke.sh "$@"
