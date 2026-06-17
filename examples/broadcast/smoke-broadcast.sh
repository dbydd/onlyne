#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
cd -- "$SCRIPT_DIR"
if [[ "${1:-}" == "--local-check" ]]; then bash -n "$SCRIPT_DIR/smoke-broadcast.sh"; python3 -m py_compile "$SCRIPT_DIR/../shared/send-many.py"; echo local-check ok; exit 0; fi
"$SCRIPT_DIR/../shared/send-many.py"
