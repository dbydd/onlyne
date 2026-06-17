#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
ONLYNE_CHANNEL=qqbot ONLYNE_ENV_PREFIX=QQBOT ../shared/channel-smoke.sh "$@"
