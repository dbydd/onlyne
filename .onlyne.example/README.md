# Onlyne v1.0.0 example tree

v1.0.0 has two roots.

Server root, passed as `onlyne-server run --root <dir>`:

```
<root>/.onlyne/
  spec.toml              # single central truth (§5)
  state.db               # ledger, WAL on
  run/s                  # admin unix socket, 0600, local user only
  run/server.pid
  logs/server.log
  keys/server.key        # ed25519 + TLS private key, PEM 0600
  templates/<topology>/<role>/   # generate content source (§11)
  ws/<topology>/<role>/  # generate default output, movable as a directory
  cache/                 # gateway render scratch files
```

Role workspace, passed as `onlyne-client run --workspace <dir>`:

```
<workspace>/.onlyne/
  config.toml            # role identity, server endpoint, local plugin list
  client.db              # session state, intents, inbox cursor
  run/s                  # local client socket
  run/client.pid
  logs/client.log
  keys/role.key          # this role private key
  agent/<pkg>/           # vendored coding-agent package copy (§11)
```

## File mapping

| example file | real path |
| --- | --- |
| `.onlyne.example/spec.toml` | `<server-root>/.onlyne/spec.toml` |
| `.onlyne.example/templates/dev/planner/.onlyne/config.toml` | `<server-root>/.onlyne/templates/dev/planner/.onlyne/config.toml` |
| `.onlyne.example/templates/dev/planner/AGENTS.md` | `<server-root>/.onlyne/templates/dev/planner/AGENTS.md` |
| `.onlyne.example/templates/dev/builder/AGENTS.md` | `<server-root>/.onlyne/templates/dev/builder/AGENTS.md` |
| `.onlyne.example/templates/dev/reviewer/AGENTS.md` | `<server-root>/.onlyne/templates/dev/reviewer/AGENTS.md` |
| `.onlyne.example/templates/dev/planner/prompts/role.md` | `<server-root>/.onlyne/templates/dev/planner/prompts/role.md` |

## Registration flow

1. `onlyne-client init --workspace W --role R --server-root S` writes `W/.onlyne/keys/role.key` and
   `W/.onlyne/config.toml`, then prints a paste-ready fragment whose first line is exactly
   `[[client]]`.
2. Append that fragment to `S/.onlyne/spec.toml`.
3. `onlyne --server-root S reload` re-parses the spec, then swaps it atomically.

`init` never writes `spec.toml`. A key that no spec entry lists gets `error{code:"unauthorized"}`
on connect.

## Truth split

`spec.toml` is the protocol truth: role names, public keys, ACL, prose, concurrency, timeouts, and
`session_command`. The tree under `<server-root>/.onlyne/templates/` is the content truth: every
role workspace file outside the runtime artifacts. A template may carry one `.onlyne/config.toml` as
a local override fragment. Generate merges it with derived values, and derived values win.
