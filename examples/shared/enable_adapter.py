#!/usr/bin/env python3
import pathlib, sys
adapter = sys.argv[1]
p = pathlib.Path('.onlyne/config.toml')
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
