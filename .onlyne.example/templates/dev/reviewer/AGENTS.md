<!--
The coding-agent package vendored into this workspace is {{agent_package}}.
docs/v1-PLAN.md §11 line 389: an empty [server].agent_package combined with a template that uses
this placeholder exits 4 with `onlyne: agent_package not set in spec.toml [server]` on stderr.
-->

# reviewer workspace

This workspace serves the reviewer role.

## Local conventions

- List findings in severity order, one line per finding.
- Record one verdict per task through the session command.
