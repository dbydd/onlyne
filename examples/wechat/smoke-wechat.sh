#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
if [[ "${1:-}" != "--local-check" ]]; then
  ../../target/debug/onlyne init >/dev/null
  ENV_PATH=$(python3 - <<'PY2'
from pathlib import Path
p = Path.cwd()
for d in [p, *p.parents]:
    if (d / '.onlyne').is_dir():
        print(d / '.onlyne' / '.env')
        break
PY2
)
  if ! grep -Eq '^WEIXIN_ILINK_TOKEN=.+' "$ENV_PATH" 2>/dev/null; then
    cat >&2 <<'MSG'
No WEIXIN_ILINK_TOKEN found in .onlyne/.env
Run first:
  ../../target/debug/onlyne auth wechat
  # or
  ../../target/debug/onlyne auth wechat --token '<token>'
MSG
    exit 2
  fi
fi
ONLYNE_CHANNEL=wechat ONLYNE_ENV_PREFIX=WECHAT ../shared/channel-smoke.sh "$@"
