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

`run` is the only launch verb, and it stays in the foreground. `--workspace` takes a relative path and resolves it to an absolute path before use, so the daemon, its generated sessions, and herdr's `--cwd` all read one location. A supervisor that wants the client in the background owns that decision — a visible terminal tab, `launchd`, `nohup` — so the client never detaches, writes no pid file, and nothing signals it by number. A `run` whose adapter socket cannot be bound ends there with exit 1 and names the failure on stderr; an `accept` error after a successful bind logs at `error` level (`adapter socket accept failed; retrying`) and retries every 100 ms with the listener held.
`status` prints `onlyne: client running uptime <n>s socket <path> faults <n>`. The `<path>` is the served socket path read through the owner tree — the canonical `run/s`, or the short derived path a deep workspace serves from, the answer `<workspace>/.onlyne/run/socket` also carries. The uptime is the age of the socket file, and a client counts as running only when that socket answers an `admin` `hello`, so a socket file an unclean exit left behind reads as not running. When the answering client holds no server link it adds `onlyne: client not connected` on stderr.

The printed `[[client]]` fragment is a complete role entry: it carries `role`, `key`, `admin`, `max_sessions`, the ACL lists, `prose`, and `session_command`. Paste it into `spec.toml` and reload; the client can then spawn sessions for that role.

## Workspace layout

Both `init` and `run` create these paths under `--workspace`:

| path | mode | content |
| --- | --- | --- |
| `.onlyne/config.toml` | | role, `cert_pin`, `key_path`, `plugins = [...]`, `[server]` host and port, `[orca]` worktree |
| `.onlyne/client.db` | | SQLite: `intents`, `sessions`, `faults`, `prose_cache`, `config_cache`, `events` |
| `.onlyne/keys/role.key` | `0600` | 32 raw ed25519 bytes, generated once |
| `.onlyne/run/` | `0700` | runtime directory |
| `.onlyne/run/s` | `0600` | adapter socket, the canonical spelling; `run` binds it while the path fits 103 bytes |
| `.onlyne/run/socket` | `0600` | one line naming the path actually served — the canonical `run/s`, or, for a tree deeper than the bound, a short derived path under the system temporary directory |
| `.onlyne/logs/client.log` | | stdout and stderr, when the operator starts `run` under a shell that redirects them |
| `.onlyne/agent/<id>/` | | installed plugin package with `plugin.toml` |
| `.onlyne/cache/orca-tabs.jsonl` | | append-only Orca tab to session map: a supervisor/display side-channel, not the identity (the adapter protocol owns that) |

`init` never writes `spec.toml`. A workspace holding the pre-v1 layout is refused before any write: exit 2 and the byte-exact line `onlyne: legacy workspace layout; v1.0.0 does not migrate`.

Three config values take a `$NAME` spelling: `cert_pin`, `key_path`, and `[server] host`. At startup `run` reads the environment variable named after the `$`, then puts its value where the config line sits. The gateway plugins use that same idiom for platform tokens. A name the environment carries no value for — absent, or present and blank — stops the launch with exit 1 and one line on stderr naming both the field and the variable: `onlyne-client: missing secret $ONLYNE_CERT for cert_pin; set the environment variable`. A value with no leading `$` travels verbatim, so a literal `$` inside a value stays part of the string.

## Exit codes

| code | meaning |
| --- | --- |
| 0 | the verb finished |
| 1 | the verb failed; the reason is one line on stderr |
| 1 | `run` could not bind the adapter socket; stderr is `onlyne-client: bind the workspace socket <canonical path>: <detail>`, the detail naming the served path, both byte lengths, and the OS reason |
| 2 | `status` found no client answering its socket, printed as `onlyne: client not running` |
| 2 | `status` found a client with no server link, printed as `onlyne: client not connected` |
| 2 | the workspace holds the legacy layout |
| 5 | `run` selected no host; stderr is `onlyne: no supported host detected; run inside herdr, orca, or zellij, or set ONLYNE_BACKEND` |

`status` exits 0 only for a client that is up and connected to its server. That is the fact a script reads.
`doctor` exits 0 for every host-detection result, including `host: null`.

## Backends

Selection is env `ONLYNE_BACKEND` (nonempty) > workspace `config.toml` `backend` > auto.

| name | parse aliases | how it is chosen | notes |
| --- | --- | --- | --- |
| `herdr` | | env, config, or auto probe (first) | pane host |
| `orca` | | env, config, or auto probe | tab host |
| `zellij` | | env, config, or auto probe | pane host; probe maps EXITED / `exit_status` |
| `exec` | `headless` | env or config only | projections record the backend as `exec` |
| `fake` | | env or config only | in-process, for tests |
| `auto` | empty string | default when env and config are empty | probes herdr, then orca, then zellij |

A nonempty value that names `herdr`, `orca`, `zellij`, `exec`/`headless`, or `fake` selects that backend. `exec` and `fake` are never discovered by auto. With no match, `onlyne-client run` exits 5 and writes `onlyne: no supported host detected; run inside herdr, orca, or zellij, or set ONLYNE_BACKEND`.

`fake` runs sessions in-process and needs no external tool; the end-to-end scripts set `ONLYNE_BACKEND=fake`. `exec` spawns the role's `session_command` as a child of the client, holds stdin open, and appends the child's output to `.onlyne/logs/session-<task>.log`. On child exit, `probe` may fill `detail.output_tail` (at most 200 lines / 16 KiB). `crates/onlyne-testkit/e2e/pi-live.sh` and `exec-headless.sh` set this path. Windows close uses `CREATE_NEW_PROCESS_GROUP` plus `CTRL_BREAK`, then `kill`; a process with no console terminates the child directly. Operator-facing graceful stop of the daemons is `onlyne shutdown`.

### herdr

A herdr session is inherited from the client process environment; a pi child running in a pane inherits it too. One server root/topology maps to one herdr workspace labelled `onlyne:<cluster>`. `<cluster>` is the server's own `[server] name`, which the client reads from `welcome.cluster` and passes to every pane it creates as `ONLYNE_CLUSTER`. One role maps to one tab. One onlyne session maps to one pane. Close is `herdr pane close`, and a `pane_not_found` answer is that close succeeding, logged `herdr pane already closed` at debug. Ids look like `wF`, `wF:t1`, `wF:p1`. A named session such as `onlyne-test` is the `HERDR_SESSION` value already in the client environment. The backend addresses a workspace by the label `onlyne:<cluster>` and a tab by the role's own name. An operator who wants a particular workspace or tab used renames it before the client spawns sessions: `herdr workspace rename <WORKSPACE_ID> onlyne:<cluster>` and `herdr tab rename <TAB_ID> <role>`. A workspace label that differs yields a second workspace, a tab name that differs yields a second tab, and the client logs a warning naming the label and the created workspace each time it takes that create path.

The client persists that address on the `sessions` row as `backend_ref`:

```json
{"herdr":{"workspace_id":"wF","tab_id":"wF:t1","pane_id":"wF:p1","agent":"onlyne-planner-abcd1234","workspace_label":"onlyne:lab","base_pane":"wF:p1","split_direction":"right"}}
```

Spawn uses two tracks. When the first token of `session_command` matches a known agent name (`pi`, `omp`, and the rest of herdr's `--kind` table), the backend runs `herdr agent start <name> --kind <k> --pane <id> --timeout 25000 -- --session-id <id> --session-dir .pi/sessions`: `--kind` selects the executable named by token 0, and the remaining `session_command` tokens travel after the `--` separator, the call shape herdr 0.9.0 documents. Commands whose first token is absent from that table run `herdr pane run <pane_id> '<one shell line>'`. `pane run` emits no JSON. The command is `shell_quote`d into a single argv token. The client injects `ONLYNE_SOCKET`, the served adapter-socket path, into every session it spawns, so a shell inside a role pane reaches `onlyne` verbs without spelling the socket. `workspace create`, `tab create`, and `pane split` pass `--cwd` absolute, the spelling herdr resolves against its own working directory.

Split direction is `PanePlacement::from_pane_count`. `(count + 1).is_power_of_two()` maps to `right`. Remaining counts map to `down`. Ratio is `0.5`. `count` is `result.tabs[].pane_count` from `herdr tab list --workspace W`. A missing field is `0`. The production spawn path passes `placement: None`, so the backend reads that live count.

Focus issues `herdr workspace focus <workspace_id>`, then `herdr tab focus <tab_id>` (positional arguments; the tab restores its last focused pane). A managed-agent pane then takes `herdr agent focus <pane_id>`. `agent focus` accepts a managed agent. A shell pane from `pane run` answers `agent_not_found`, so that track walks `herdr pane focus --pane <base_pane> --direction <split_direction>`: the neighbour of the anchor the split recorded. `herdr pane get <pane_id>` is the confirmation step; `result.pane.focused` must be true, and a hop landing elsewhere reports the pane holding focus. The control plane is `ControlOp::Focus{task_id}`; a failed backend `focus()` records `Report::Fault{kind:"focus"}`. CLI: `onlyne control --from <role> focus --task <id>`. TUI: `F`.

A role at `max_sessions` keeps pulling with `control_only`, which is the path that lets `focus`, `recycle`, and `cancel` reach the session occupying the last free slot.

An agent that mounts naming no session — the always-running plugin — parks as the connection for the next staged session. The claim binds that socket to the session it takes, and a mount that arrives after a work item hands that item over on the spot. A connection that named no session releases only the transports sharing its socket.

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

`max_sessions` from the role's spec entry caps how many sessions a role runs at once. A session whose stored lifecycle reads `exited` spends none of that cap: the rows of sessions the role has ended stay in `client.db` as its history and stay queryable. The client keeps pulling while fewer than `max_sessions` sessions have not exited. Each task gets its own session and its own spawn. A session that has finished one task takes no further task; its slot releases, its host resource closes, and it stops counting against `max_sessions`.

The host resource retires with the session: a pane, tab, zellij session, or exec child closes once that session holds no task and no plugin transport is attached. Three paths do the closing — a graceful plugin `detach` closes each idle session that connection served, a settle with no attached agent closes at settle time, and the 250 ms readiness tick closes any tracked session whose stored lifecycle projects `exited` with a stored outcome while its agent is gone, taking the reason from that outcome (`Completed`, `Fault`, or `Cancelled`). One case keeps the resource: a connection that dropped without a `detach`, where that agent may reconnect. Past `[client] reconnect_grace_secs` that agent is gone, and the sweep settles the task the session still owed `failed` and refuses that task's delivery with reason `session_dead`: the row leaves `in_flight`, so the ledger carries the ending an operator reads and `repair retry` is what brings the work back. The same pass publishes the session's own projection — the heartbeat report every ordinary ending travels — so the server's mirrored row for it reads `exited` at once, instead of reading `working` until the server's stale observer records a `stale_working` or `heartbeat_missing` fault. A retirement with the stored resource still open refreshes a stale `backend_ref` through `attach`, projects `resource_closed`, logs `retiring idle session resource` with task, backend, resource, and reason, then closes the resource and drops the slot; a close that fails is a warning.

## Server link

The client reconnects on a ladder of 1, 2, 4, 8, 16, 32, 60 seconds; 60 seconds repeats for every later attempt. After a reconnect the order is handshake, welcome, intent flush, pull resume.

A link failure or a `bye` frame sets `accept_new = false`. Queued deliveries wait on the server, running sessions continue to their terminal state, and the completions those sessions produce enter the intent queue. `accept_new = false` blocks new session spawns and new pulls. The gate follows the connection rather than any one frame: the runloop sets it from the link's readiness, and a frame that could not leave — a request past its own deadline with the link still up — goes to the intent queue alone.

A delivery the pull already had in hand when the gate shut is left unanswered: the row stays in flight and the next `hello` requeues it. A refusal would settle that row `rejected`, which is terminal, so the work would come back only through an operator's `repair retry`.

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
