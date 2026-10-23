# planner workspace

This workspace serves the planner role. This role's admin flag is `{{admin}}`.

## Local conventions

- Keep one task in flight per session. The only exception is a session that is idle and unbound.
- Read the role prompt that arrives with `welcome`. The workspace holds no prose copy.
- Write artifacts under this workspace root. The server moves no files between workspaces.
