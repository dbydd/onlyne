#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
ONLYNE_CHANNEL=feishu ONLYNE_ENV_PREFIX=FEISHU ../shared/channel-smoke.sh "$@"
