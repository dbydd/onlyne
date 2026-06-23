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
command -v magick >/dev/null || { echo "ImageMagick 'magick' required" >&2; exit 127; }
tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT
src="$tmpdir/table.md"
svg="$tmpdir/table.svg"
cat > "$src"
python3 - "$src" "$svg" <<'PY'
from pathlib import Path
from xml.sax.saxutils import escape
import sys

src, svg = map(Path, sys.argv[1:3])
lines = [l.strip() for l in src.read_text().splitlines() if l.strip()]
rows = []
for line in lines:
    if '|' not in line:
        continue
    cells = [c.strip() for c in line.strip('|').split('|')]
    if cells and all(set(c.replace(':', '').strip()) <= {'-'} and '-' in c for c in cells):
        continue
    rows.append(cells)
if not rows:
    rows = [[l] for l in lines or ['']]
cols = max(len(r) for r in rows)
for r in rows:
    r += [''] * (cols - len(r))

def units(s):
    return sum(2 if ord(ch) > 127 else 1 for ch in s)

col_w = []
for i in range(cols):
    col_w.append(max(120, min(460, max(units(r[i]) for r in rows) * 15 + 36)))
row_h = 58
pad = 24
width = sum(col_w) + pad * 2
height = row_h * len(rows) + pad * 2
x_edges = [pad]
for w in col_w:
    x_edges.append(x_edges[-1] + w)
parts = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">', '<rect width="100%" height="100%" fill="white"/>']
for ri, row in enumerate(rows):
    y = pad + ri * row_h
    fill = '#f3f4f6' if ri == 0 else '#ffffff'
    weight = '700' if ri == 0 else '400'
    for ci, cell in enumerate(row):
        x = x_edges[ci]
        w = col_w[ci]
        parts.append(f'<rect x="{x}" y="{y}" width="{w}" height="{row_h}" fill="{fill}" stroke="#d0d7de" stroke-width="2"/>')
        parts.append(f'<text x="{x + 16}" y="{y + 37}" fill="#111111" font-size="24" font-weight="{weight}">{escape(cell)}</text>')
parts.append('</svg>')
svg.write_text('\n'.join(parts))
PY
font="/System/Library/Fonts/Hiragino Sans GB.ttc"
[[ -f "$font" ]] || font="/System/Library/Fonts/Menlo.ttc"
magick -font "$font" -background white "$svg" -alpha remove -alpha off "PNG24:$out"
