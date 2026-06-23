#!/usr/bin/env bash
set -euo pipefail
out=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) out=${2:?}; shift 2 ;;
    *) shift ;;
  esac
done
[[ -n "$out" ]] || { echo "--out required" >&2; exit 2; }
tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT
src="$tmpdir/table.md"
cat > "$src"
if command -v qlmanage >/dev/null; then
  qlmanage -t -s 1200 -o "$tmpdir" "$src" >/dev/null 2>&1
  cp "$src.png" "$out"
  exit 0
fi
if command -v magick >/dev/null; then
  magick -background white -fill '#111111' -pointsize 22 -size 1200x "caption:@$src" "$out"
  exit 0
fi
echo "need qlmanage or ImageMagick magick" >&2
exit 127
