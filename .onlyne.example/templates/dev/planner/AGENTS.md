# planner workspace

This workspace serves the planner role. The admin flag of this role is `{{admin}}`.

## Local conventions

- Keep one task in flight per session unless that session is idle and unbound.
- Read the role prompt delivered with `welcome`; the workspace holds no prose copy.
- Write artifacts under this workspace root. The server moves no files between workspaces.
