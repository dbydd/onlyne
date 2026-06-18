#!/usr/bin/env python3
import pathlib, sys
adapter = sys.argv[1]

start = pathlib.Path.cwd()
for d in (start, *start.parents):
    p = d / '.onlyne/config.toml'
    if p.exists():
        break
else:
    raise SystemExit('no .onlyne/config.toml found; run onlyne init first')
s = p.read_text()
marker = f'[adapters.{adapter}]'
i = s.index(marker)
j = s.find('\n[', i + 1)
if j == -1:
    j = len(s)
block = s[i:j]
lines = block.splitlines()
for n, line in enumerate(lines):
    if line.startswith('enabled = '):
        lines[n] = 'enabled = true'
        break
else:
    lines.insert(1, 'enabled = true')
p.write_text(s[:i] + '\n'.join(lines) + s[j:])
