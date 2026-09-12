# onlyne-client

One workspace, one role, one daemon. The role runs many sessions at once.

## Verbs

| verb | one line |
| --- | --- |
| `run --workspace <dir>` | Foreground role runtime: connect, handshake, pull, dispatch, report. Backgrounding is the operator's job, never the client's. |
| `status --workspace <dir>` | Print uptime, socket path, recorded fault count, and whether the server link is up. |
| `init --workspace <dir> --role <r> --server-root <dir>` | Build the minimal role workspace and print the `[[client]]` spec fragment. |
| `roles --workspace <dir>` | Answer role prose from the local cache. |
| `sessions --workspace <dir>` | Reserved for the live role runtime. |
| `watch --workspace <dir>` | Reserved for the live role runtime. |
| `history --workspace <dir>` | Reserved for the live role runtime. |

`run` is the only launch verb, and it stays in the foreground. A supervisor that wants it in the background owns that decision — a visible terminal tab, `launchd`, `nohup` — so the client never detaches, writes no pid file, and nothing signals it by number.
`status` prints `onlyne: client running uptime <n>s socket <path> faults <n>`. The uptime is the age of the socket file, and a client counts as running only when that socket answers an `admin` `hello`, so a socket file an unclean exit left behind reads as not running. When the answering client holds no server link it adds `onlyne: client not connected` on stderr.

The printed `[[client]]` fragment is a complete role entry: it carries `role`, `key`, `admin`, `max_sessions`, the ACL lists, `prose`, `reuse`, and `session_command`. Paste it into `spec.toml` and reload; the client can then spawn sessions for that role.

## Workspace layout

Both `init` and `run` create these paths under `--workspace`:

| path | mode | content |
| --- | --- | --- |
| `.onlyne/config.toml` | | role, `cert_pin`, `key_path`, `[server]` host and port, `[orca]` worktree, `[[plugin]]` entries |
| `.onlyne/client.db` | | SQLite: `intents`, `sessions`, `faults`, `prose_cache`, `config_cache`, `events` |
| `.onlyne/keys/role.key` | `0600` | 32 raw ed25519 bytes, generated once |
| `.onlyne/run/` | `0700` | runtime directory |
| `.onlyne/run/s` | `0600` | adapter socket, bound by `run` |
| `.onlyne/logs/client.log` | | stdout and stderr, when the operator starts `run` under a shell that redirects them |
| `.onlyne/agent/<id>/` | | installed plugin package with `plugin.toml` |
| `.onlyne/cache/orca-tabs.jsonl` | | append-only Orca tab to session map: a supervisor/display side-channel, not the identity (the adapter protocol owns that) |

`init` never writes `spec.toml`. A workspace holding the pre-v1 layout is refused before any write: exit 2 and the byte-exact line `onlyne: legacy workspace layout; v1.0.0 does not migrate`.

## Exit codes

| code | meaning |
| --- | --- |
| 0 | the verb finished |
| 1 | the verb failed; the reason is one line on stderr |
| 2 | `status` found no client answering its socket, printed as `onlyne: client not running` |
| 2 | `status` found a client with no server link, printed as `onlyne: client not connected` |
| 2 | the workspace holds the legacy layout |

`status` exits 0 only for a client that is up and connected to its server. That is the fact a script reads.

## Backends

`ONLYNE_BACKEND` picks the session backend: `auto`, `zellij`, `orca`, `fake`, `exec`. An empty value discovers by capability: the client probes `orca`, then `zellij`, then `fake`, and takes the first one that reports usable. `auto` behaves the same. `fake` runs sessions in process and needs no external tool, which is why the end-to-end scripts use it. `exec` spawns the role's `session_command` as a child of the client, holds stdin open, and appends the child's output to `.onlyne/logs/session-<task>.log`. It is never reached through `auto`: running an agent with no terminal around it is a deliberate choice for a headless host (`crates/onlyne-testkit/e2e/pi-live.sh` makes it), not a fallback to discover.

`[orca] worktree` in `config.toml` sets which Orca tab list a session tab joins. Three states:

* `host` (the default) reads `ORCA_WORKTREE_ID`, the worktree id Orca exports to the tab the supervisor started the client in and which the daemon inherits. Every session tab lands flat in that worktree's tab list, beside the supervisor's own tabs. Start the client outside an Orca tab and the variable is absent, so the policy behaves like `inherit`.
* `inherit` passes no selector and leaves the choice to Orca's active worktree.
* Any other value is used verbatim as an Orca worktree selector (`id:<…>`, `path:<abs>`, `name:<…>`, `branch:<…>`).

Tab ownership and working directory are independent. The selector decides which tab list the tab joins; the spawned command's own `cd` decides where the agent runs. The role workspace therefore never has to exist in Orca: it is not registered, not opened, and not cleaned up. That is the whole reason a generated (non-git) role workspace works at all. Orca's public registration command accepts git checkouts only, so a `path:<workspace>` selector would fail for exactly the directories this client hands out.

## Sessions

`max_sessions` from the role's spec entry caps how many sessions a role runs at once. A session whose stored lifecycle reads `exited` spends none of that cap: the rows of sessions the role has ended stay in `client.db` as its history and stay queryable. The client keeps pulling while fewer than `max_sessions` sessions have not exited. `reuse = true` keeps a settled session's slot for the role's next task; `reuse = false` gives the slot back when the task settles.

## Server link

The client reconnects on a ladder of 1, 2, 4, 8, 16, 32, 60 seconds; 60 seconds repeats for every later attempt. After a reconnect the order is handshake, welcome, intent flush, pull resume.

A link failure or a `bye` frame sets `accept_new = false`. Queued deliveries wait on the server, running sessions continue to their terminal state, and the completions those sessions produce enter the intent queue. `accept_new = false` blocks new session spawns and new pulls.

## Intent queue

Every outbound envelope lands in `client.db` `intents` before the first socket write, keyed by `op_id`.

| state | meaning | next state |
| --- | --- | --- |
| `pending` | enqueued, first attempt owed | `accepted`, `retrying`, `exhausted` |
| `retrying` | waiting for a later attempt | `accepted`, `retrying`, `exhausted` |
| `accepted` | receipt stored | terminal |
| `exhausted` | attempt ceiling reached with a recorded fault | terminal |

The role spec sets the attempt ceiling in `intent.attempts` and the delay between attempts in `intent.backoff_ms`. The queue lives in the client database, so a restart resumes it.

| answer | rule |
| --- | --- |
| `ok = true` | store the receipt, mark `accepted` |
| `duplicate` | replay the stored receipt from the first attempt |
| `acl_denied`, `invalid`, `conflict`, `forbidden`, `unknown_role`, `not_admin`, `bad_frame`, `frame_too_large`, `protocol_version` | drop the row without a retry |
| `internal`, connection loss | count the attempt and retry after the ladder |

Hitting the ceiling records fault kind `intent_exhausted` and sends `report{kind:"fault"}` once the server link exists. An exhausted row is never dropped silently.

## Role prose

`welcome.prose` is cached in `prose_cache`, keyed by role with `spec_hash`. `roles` and the cluster prose export read that record.
