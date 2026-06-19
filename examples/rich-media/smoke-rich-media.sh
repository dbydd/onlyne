#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
cd -- "$SCRIPT_DIR"
if [[ "${1:-}" == "--local-check" ]]; then
  bash -n "$SCRIPT_DIR/$(basename "$0")"
  python3 -m py_compile "$SCRIPT_DIR/../shared/send-many.py"
  echo local-check ok
  exit 0
fi
: "${ONLYNE_TARGETS:?set ONLYNE_TARGETS as channel:conversation[,channel:conversation...]}"
export ONLYNE_FORMAT=${ONLYNE_FORMAT:-markdown}
export ONLYNE_TEXT=${ONLYNE_TEXT:-'# Onlyne rich message smoke

- **bold** and _italic_
- `inline code`
- [Onlyne](https://github.com/dbydd/onlyne)

```rust
println!("hello rich media");
```'}
"$SCRIPT_DIR/../shared/send-many.py"
