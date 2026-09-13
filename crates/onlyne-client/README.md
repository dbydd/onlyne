# onlyne-client

One workspace, one role, one daemon. The role runs many sessions at once.

## Verbs

| verb | one line |
| --- | --- |
| `run --workspace <dir>` | Foreground role runtime: connect, handshake, pull, dispatch, report. Backgrounding is the operator's job, never the client's. |
| `status --workspace <dir>` | Print uptime, socket path, recorded fault count, and whether the server link is up. |
| `doctor` | Print host-detection JSON. No workspace, no socket. Exit 0. |
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
| 5 | `run` selected no host; stderr is `onlyne: no supported host detected; run inside herdr, orca, or zellij, or set ONLYNE_BACKEND` |

`status` exits 0 only for a client that is up and connected to its server. That is the fact a script reads.
`doctor` exits 0 for every host-detection result, including `host: null`.

## Backends

`ONLYNE_BACKEND` selects the session backend. The name set is `herdr | orca | zellij | exec | fake | auto`. A nonempty value that names `herdr`, `orca`, `zellij`, `exec`, or `fake` selects that backend. An empty value or `auto` probes herdr, then orca, then zellij. `exec` and `fake` enable only when `ONLYNE_BACKEND` names them. With no match, `onlyne-client run` exits 5 and writes `onlyne: no supported host detected; run inside herdr, orca, or zellij, or set ONLYNE_BACKEND`.

`fake` runs sessions in-process and needs no external tool; the end-to-end scripts set `ONLYNE_BACKEND=fake`. `exec` spawns the role's `session_command` as a child of the client, holds stdin open, and appends the child's output to `.onlyne/logs/session-<task>.log`. `crates/onlyne-testkit/e2e/pi-live.sh` sets `ONLYNE_BACKEND=exec` for a headless host.

### herdr

A herdr session is inherited from the client process environment; a pi child running in a pane inherits it too. One server root/topology maps to one herdr workspace labelled `onlyne:<cluster>`. `<cluster>` is the server's own `[server] name`, which the client reads from `welcome.cluster` and passes to every pane it creates as `ONLYNE_CLUSTER`. One role maps to one tab. One onlyne session maps to one pane. Close is `herdr pane close`. Ids look like `wF`, `wF:t1`, `wF:p1`. A named session such as `onlyne-test` is the `HERDR_SESSION` value already in the client environment.

The client persists that address on the `sessions` row as `backend_ref`:

```json
{"herdr":{"workspace_id":"wF","tab_id":"wF:t1","pane_id":"wF:p1","agent":"onlyne-planner-abcd1234","workspace_label":"onlyne:lab","base_pane":"wF:p1","split_direction":"right"}}
```

Spawn uses two tracks. When the first token of `session_command` matches a known agent name (`pi`, `omp`, and the rest of herdr's `--kind` table), the backend runs `herdr agent start <name> --kind <k> --pane <id> --timeout 25000`. Commands whose first token is absent from that table run `herdr pane run <pane_id> '<one shell line>'`. `pane run` emits no JSON. The command is `shell_quote`d into a single argv token.

Split direction is `PanePlacement::from_pane_count`. `(count + 1).is_power_of_two()` maps to `right`. Remaining counts map to `down`. Ratio is `0.5`. `count` is `result.tabs[].pane_count` from `herdr tab list --workspace W`. A missing field is `0`. The production spawn path passes `placement: None`, so the backend reads that live count.

Focus issues `herdr workspace focus <workspace_id>`, then `herdr tab focus <tab_id>` (positional arguments; the tab restores its last focused pane). A managed-agent pane then takes `herdr agent focus <pane_id>`. `agent focus` accepts a managed agent. A shell pane from `pane run` answers `agent_not_found`, so that track walks `herdr pane focus --pane <base_pane> --direction <split_direction>`: the neighbour of the anchor the split recorded. `herdr pane get <pane_id>` is the confirmation step; `result.pane.focused` must be true, and a hop landing elsewhere reports the pane holding focus. The control plane is `ControlOp::Focus{task_id}`; a failed backend `focus()` records `Report::Fault{kind:"focus"}`. CLI: `onlyne control --from <role> focus --task <id>`. TUI: `F`.

A role at `max_sessions` keeps pulling with `control_only`, which is the path that lets `focus`, `recycle`, and `cancel` reach the session occupying the last free slot.

An agent that mounts naming no session — the always-running plugin — parks as the connection for the next staged session. The claim binds that socket to the session it takes, so every later task a `reuse` role hands the same session rides it, and a mount that arrives after a work item hands that item over on the spot. A connection that named no session releases only the transports sharing its socket.

### doctor

`onlyne-client doctor` is a read-only verb. It prints one JSON object and exits 0. Fields:

| field | meaning |
| --- | --- |
| `host` | selected backend name, or `null` |
| `backend_selection` | `explicit`, `env`, or `none` |
| `explicit` | raw `ONLYNE_BACKEND` when nonempty |
| `binary` | CLI path or name for herdr/orca/zellij; `null` for exec, fake, and no host |
| `session` | `HERDR_SESSION` |
| `workspace_id` | `HERDR_WORKSPACE_ID` |
| `tab_id` | `HERDR_TAB_ID` |
| `pane_id` | `HERDR_PANE_ID` |
| `refusal` | the `NO_SUPPORTED_HOST` line, present when `host` is `null` |

A missing host yields `host: null` plus `refusal` and exit 0. The verb is a pre-deploy check.

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
