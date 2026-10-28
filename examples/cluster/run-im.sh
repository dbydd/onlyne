#!/usr/bin/env bash
# Starts one IM channel of the cluster experiment.
#
# The gateway is not packaged as a CLI yet, so this runs the package entry through bun.
# The configuration holds repo-root-relative paths, so the process starts from the root.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"

# The credential comes from FEISHU_CREDENTIAL in the environment. A local .env beside this
# script is sourced when present; no credential file inside the repository is consulted.
env_file="$here/.env"
if [[ -f "$env_file" ]]; then
  set -a
  # shellcheck disable=SC1090,SC1091
  source "$env_file"
  set +a
  printf 'run-im: environment from %s\n' "${env_file#"$root"/}" >&2
fi

channel="${1:-lark}"
case "$channel" in
  lark|qq|wechat) ;;
  *) printf 'usage: run-im.sh [lark|qq|wechat]\n' >&2; exit 2 ;;
esac
cd "$root"
exec bun packages/im-gateway/src/main.ts "examples/cluster/gateways/im-$channel.json"
