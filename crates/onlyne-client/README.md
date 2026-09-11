# onlyne-client

One workspace, one role, one daemon. Many concurrent sessions inside the role.

## Verbs

| verb | one line |
| --- | --- |
| `run --workspace <dir>` | Foreground role runtime: connect, handshake, pull, dispatch, report. |
| `start --workspace <dir>` | Spawn `run` detached, log to `.onlyne/logs/client.log`, record the pid, answer once the socket is bound. |
| `stop --workspace <dir>` | Signal the recorded pid, wait for it to leave, remove the pid file and the socket. |
| `status --workspace <dir>` | Print pid, uptime, socket path, and the recorded fault count. |
| `init --workspace <dir> --role <r> --server-root <dir>` | Build the minimal role workspace and print the `[[client]]` spec fragment. |
| `roles --workspace <dir>` | Answer role prose from the local cache. |
| `sessions --workspace <dir>` | Reserved for the live role runtime. |
| `watch --workspace <dir>` | Reserved for the live role runtime. |
| `history --workspace <dir>` | Reserved for the live role runtime. |

`start` prints `onlyne: client started pid <pid> socket <path>`.
`status` prints `onlyne: client running pid <pid> uptime <n>s socket <path> faults <n>`.
`stop` prints `onlyne: client stopped pid <pid>`.

## Workspace layout

`init` and `run` create these paths under `--workspace`:

| path | mode | content |
| --- | --- | --- |
| `.onlyne/config.toml` | | role, `cert_pin`, `key_path`, `[server]` host and port, `[orca]` worktree, `[[plugin]]` entries |
| `.onlyne/client.db` | | SQLite: `intents`, `sessions`, `faults`, `prose_cache`, `config_cache`, `events` |
| `.onlyne/keys/role.key` | `0600` | 32 raw ed25519 bytes, generated once |
| `.onlyne/run/` | `0700` | runtime directory |
| `.onlyne/run/s` | `0600` | adapter socket, bound by `run` |
| `.onlyne/run/client.pid` | `0600` | pid written by `start`, removed by `stop` |
| `.onlyne/logs/client.log` | | stdout and stderr of the `start` child |
| `.onlyne/agent/<id>/` | | installed plugin package with `plugin.toml` |
| `.onlyne/cache/orca-tabs.jsonl` | | append-only Orca tab to session map: a supervisor/display side-channel, not the identity (the adapter protocol owns that) |

`init` never writes `spec.toml`. A workspace holding the pre-v1 layout is
refused before any write, with exit 2 and the byte-exact line
`onlyne: legacy workspace layout; v1.0.0 does not migrate`.

## Exit codes

| code | meaning |
| --- | --- |
| 0 | the verb finished |
| 1 | the verb failed; the reason is one line on stderr |
| 2 | `stop` or `status` found no running client, printed as `onlyne: client not running` |
| 2 | the workspace holds the legacy layout |

## Backends

`ONLYNE_BACKEND` selects the session backend: `auto`, `zellij`, `orca`, `fake`.
The default is `zellij`. `auto` probes zellij, orca, then fake and takes the
first one that reports usable. `fake` runs sessions in process and needs no
external tool, which is why the end-to-end scripts use it.

`[orca] worktree` in `config.toml` says which Orca tab list a session tab
joins. Three states:

* `host` (the default) reads `ORCA_WORKTREE_ID`, the worktree id Orca exports
  to the tab the supervisor started the client in and which the daemon
  inherits. Every session tab lands flat in that worktree's tab list, beside
  the supervisor's own tabs. Started outside an Orca tab the variable is
  absent and the policy behaves like `inherit`.
* `inherit` passes no selector, leaving the choice to Orca's active worktree.
* Any other value is used verbatim as an Orca worktree selector
  (`id:<…>`, `path:<abs>`, `name:<…>`, `branch:<…>`).

Tab ownership and working directory are independent: the selector decides
which tab list the tab joins, while the spawned command's own `cd` decides
where the agent runs. The role workspace therefore never has to exist in Orca
— it is not registered, not opened, and not cleaned up — which is the whole
reason a generated (non-git) role workspace works at all. Orca's public
registration command accepts git checkouts only, so a `path:<workspace>`
selector would fail for exactly the directories this client hands out.

## Server link

The client reconnects on a ladder of 1, 2, 4, 8, 16, 32, 60 seconds, and 60
seconds repeats for every later attempt. After a reconnect the order is
handshake, welcome, intent flush, pull resume.

A failure on the link or a `bye` frame sets `accept_new = false`. Queued
deliveries wait on the server, running sessions continue to their terminal
state, and completions produced by those sessions enter the intent queue.
`accept_new = false` blocks new session spawns and new pulls.

## Intent queue

Every outbound envelope lands in `client.db` `intents` before the first socket
write, keyed by `op_id`.

| state | meaning | next state |
| --- | --- | --- |
| `pending` | enqueued, first attempt owed | `accepted`, `retrying`, `exhausted` |
| `retrying` | waiting for a later attempt | `accepted`, `retrying`, `exhausted` |
| `accepted` | receipt stored | terminal |
| `exhausted` | attempt ceiling reached with a recorded fault | terminal |

The attempt ceiling comes from the role spec `intent.attempts`, and the delay
between attempts comes from `intent.backoff_ms`. The queue lives in the client
database, so a restart resumes it.

| answer | rule |
| --- | --- |
| `ok = true` | store the receipt, mark `accepted` |
| `duplicate` | replay the stored receipt from the first attempt |
| `acl_denied`, `invalid`, `conflict`, `forbidden`, `unknown_role`, `not_admin`, `bad_frame`, `frame_too_large`, `protocol_version` | drop the row without a retry |
| `internal`, connection loss | count the attempt and retry after the ladder |

The ceiling records fault kind `intent_exhausted` and sends `report{kind:"fault"}`
once the server link exists. An exhausted row is never dropped silently.

## Role prose

`welcome.prose` is cached in `prose_cache` keyed by role with `spec_hash`.
`roles` and the cluster prose export read that record.
