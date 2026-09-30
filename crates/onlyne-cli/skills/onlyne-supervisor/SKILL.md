---
name: onlyne-supervisor
description: Use when operating an Onlyne cluster as _supervisor — starting workspaces, dispatching tasks, polling the ledger, granting routes, reading faults, and repairing deliveries.
---

# Onlyne Supervisor

You own orchestration and recovery. The server routes, queues, records, and enforces ACL.
One narrow automatic rule can move a stale `working` mirror only after that task's own ledger
row is terminal; the server never settles an open task. Every other recovery choice belongs
to you and the spec file.

## Mount points

- Admin surface: `onlyne --server-root <root> <verb>` reaches the socket that root's daemon
  publishes in the machine-level runtime directory
  (`/tmp/onlyne-<uid>/<digest>.sock`, `$ONLYNE_RUNTIME_DIR` overriding it), the local trust
  root (0600); `<root>/.onlyne/run/s` is only the canonical spelling operators print. Your
  admin sends carry a required `--from _supervisor`, and the admin surface stamps `admin = true`
  on every envelope it relays. Put `admin = true` on your own `[[client]]` entry too: that
  is the standing a client-held supervisor session sends with, and the flag the Control-class
  ACL bypass reads.
- Lifecycle: `onlyne server init|run|status|generate|reload`, the top-level
  `onlyne wait-ready`, `onlyne client run|init|status`, and `onlyne gateway status`. The
  platform verbs that used to sit beside `gateway status` went with the frozen gateway
  crate; the mount vocabulary itself stays in the protocol. Nothing detaches and there is
  no `start`/`stop`: both daemons
  run in the foreground, and a supervisor that wants one in the background starts `run`
  itself (a tab, `launchd`, `systemd`).
- Every request verb prints one JSON line for its answer. `onlyne cluster export-prose`
  prints the prose raw unless `--json` wraps it in an object. Exit codes: 0 ok, 1 failed
  answer, 2 local validation, 3 no socket, 4 the operator's input was refused (`generate`,
  and `skill export` declining to overwrite a file), 5 `client run` found no session host,
  127 missing sibling.

## Current operating facts

- Install the current published registry set with:

  ```bash
  cargo install --locked \
    onlyne-cli onlyne-server onlyne-client onlyne-testkit
  ```

- `onlyne version` reports the CLI package, protocol, and sibling binary paths. The testkit
  command does not accept `--version`; read `onlyne version` for the installed package
  inventory. The TUI is a verb of the `onlyne` binary, not a separate one.
- `onlyne schema spec` and `onlyne schema client` print the compiled JSON Schema of
  `<server-root>/.onlyne/spec.toml` and `<workspace>/.onlyne/config.toml`; `--pretty` indents
  the same document. The keys, their types, and which of them are required come out of the
  build, so an installed binary answers the whole vocabulary without a source checkout.
- Re-export the role and supervisor handbooks from the installed binary with
  `onlyne skill export --set role --set supervisor --force`.
- The live acceptance shape is completion followed by handoff and client kill. Read
  the task ledger, session projection, both sequence numbers, and the ghost audit after the
  task settles. The expected readings are `acked`, `exited`, and no new ghost row for that task.

## Onboard a cluster

1. `onlyne server init --root <root> --listen 127.0.0.1:<port>` writes the `[server]` spec,
   the server key pair, and a fresh `cert_pin` in it. An existing `spec.toml` stops the verb
   with exit 4 until `--force`, and an existing readable key pair keeps its own pin.
2. Put role content in `<root>/.onlyne/templates/<topo>/<role>/`. The basename is the spec
   role name. Templates hold AGENTS.md, prompts, and `.pi/` settings — opaque bytes, plus a
   closed set of placeholders: `{{role}} {{cluster}} {{server_name}} {{listen}} {{cert_pin}} {{admin}} {{max_sessions}} {{agent_package}}`.
   The coding-agent package travels on that last placeholder, and one appearance is what
   makes it happen: set `[server].agent_package` to the absolute path of a real package (the
   spec `onlyne server init` writes leaves it an empty string), and give the template a
   `.pi/settings.json` holding `{"packages": ["{{agent_package}}"]}`. `generate` then vendors
   the whole package into `<ws>/.onlyne/agent/<pkg-name>/` and renders that entry as
   `../.onlyne/agent/<pkg-name>`. The `../` form is the one pi 0.85.1 loads: a project
   `packages` path resolves against the directory holding the settings file, so a bare
   `.onlyne/agent/<pkg-name>` lists the package and starts nothing. Anywhere else in a
   template the same placeholder renders as the workspace-relative
   `.onlyne/agent/<pkg-name>`. A template that uses it while `agent_package` is empty exits 4
   with `onlyne: agent_package not set in spec.toml [server]`; a template that never names the
   directory gets a workspace carrying no package at all, and its sessions start with no
   plugin tools registered.
   The skill a workspace carries comes out of the binary:
   `onlyne skill export --dest <workspace>/.agents/skills --set role` writes `onlyne-role`,
   `--set supervisor` writes `onlyne-supervisor` for your own seat, and `--set dev` writes
   `onlyne`, each as `<dest>/<name>/SKILL.md`. The default
   `<dest>` is `.agents/skills` under the working directory. A file whose bytes already
   match is left alone, and a differing file stops the export with exit 4 and `onlyne:
   refusing to overwrite <path>; pass --force` before anything is written.
3. `onlyne server generate --root <root>` renders workspaces under `<root>/.onlyne/ws` and
   prints paste-ready `[[client]]` fragments. If any generated byte embeds an absolute path,
   it fails: exit 4, and the output is deleted. Move a generated directory wherever you
   want — `mv`, then `onlyne client run --workspace <new-path>` in the foreground;
   whoever wants it backgrounded starts it that way. That is the whole relocation
   story.
4. Append the fragments to `spec.toml`, then run `onlyne reload`. `onlyne spec-diff` shows
   the pending delta first. The spec file is the only truth; there is no runtime config API.

An existing tree carries a store marker: the server's `state.db` names revision 6 and a
client's `client.db` names revision 3. A marker answering another revision stops that daemon
with a sentence naming the revision it found, and a pre-v1 layout stops `onlyne client init`
before it writes anything. Both exit 6, the code reserved for "this build will not start on a
file from another revision"; there is no `migrate` command, so the operator moves the old file
aside and starts again.

## Dispatch flows downhill

```bash
onlyne --server-root <root> send --from _supervisor --to <role> --text "RING=... K=1 TOTAL=10" \
  --force --yes-i-am-supervisor-not-other-role
```

Seven verbs require both flags: `send`, `reply`, `handoff`, `complete`, `ack`, `reject`, and
`control`. Each writes a role's own voice on the wire, and the pair is your declaration that the
call stands outside that role's plugin session. The refusal names the plugin tool that answers
for a role where one exists (`onlyne_send` for `send`, `onlyne_handoff` for `handoff`,
`onlyne_complete` for `complete`). A call missing either flag exits 2 before it opens a socket.
`repair *`, `ledger`, `sessions`, `roles`, `faults`, `watch`, `history`, `reload`, `status`, and
`shutdown` carry no such flag.

A task family carries its own metadata, and you set it where the run starts. `onlyne ... send
--hop-budget <n>` records the hops the family may spend, `--label <k=v>` (repeat the flag up to
eight times) records whatever a script of yours reads beside the ledger, and `--deadline
<rfc3339>` records the wall-clock bound for the whole run. Every handoff inherits all of it: the
child carries the family's root task id, its budget, its origin — the role that sent the root — its
deadline, and its labels, so the hop that meets the budget is the hop that keeps the work. The
figures ride `Causality`, so `onlyne ledger` prints `family` and `hop_budget` off the row without a
script rebuilding them from `parent_task` links. The delivery a role's model reads names neither,
and a `handoff` that would sit over the budget is refused before the child is minted: the counters
are read off the ledger, never out of the task text.

- Roles answer by completing the task. The receipt lands in the ledger as `out_head`: the
  first 200 grapheme clusters of the completion body (`head_preview` in
  `crates/onlyne-store/src/server.rs`). A pi session's `onlyne_complete` flattens its text to
  one line before it goes. `onlyne complete --summary` carries that line truncated to 200
  characters (`HEAD_CHARS` in `crates/onlyne-cli/src/verbs.rs`), and `--head-from ledger`
  reads the head back off the task's own row. `--details` carries the whole result to
  the next hop and the originator, capped at the protocol's `details` ceiling, and
  `--file` names an absolute path the result points at; a details body over the cap is
  refused before anything is sent. A receipt for a task your role dispatched
  reaches you with no receiver-side grant, and waits in `queued` until your role has a live
  session: those rows are your pull-inbox, `ledger` reads the backlog, and the queue drains
  as your own client pulls.
- The ledger carries the state. Poll it for proof.

```bash
onlyne --server-root <root> ledger --task <task-id>       # queued|in_flight|acked|rejected|expired
onlyne --server-root <root> sessions --task <task-id>     # lifecycle projection
onlyne --server-root <root> watch --follow --tier durable  # live stream; tiers: durable|advisory
```

- Your sends need no receiver grant. Leave `allowed_targets` off your own entry and every
  registered role is reachable; name a list there and the reach is exactly that list. The
  receiver's `allowed_senders` is read on no row of yours, so dispatch and repair start at
  `send`, with no spec edit first.
- `_supervisor` is your inbox, and a backlog is what an inbox is for. The entry is a logical
  signature node: `command = []`, so no client ever dials it, no session is ever opened for
  it, and it reads `offline` for the life of the cluster. That is not a broken role, not a
  leak, and not dirty data. Your seat is yours to start and stop and it is longer-lived than
  the swarm it supervises, so whatever arrived while you were gone is exactly what you came
  back for. The ledger is durable, `onlyne --server-root <root> ledger` reads the whole
  backlog, and `queued: N` on the role reads as "N items are waiting for me". Drain it first
  on every start, before you dispatch anything new.
- A supervisor that runs as a client — a real `command` naming a runtime — drains those same
  rows by pulling them into its own session, and that is the right shape when you want the
  work delivered rather than collected. It is the wrong shape when your lifetime is the
  user's to manage, because a client-held inbox only holds what its own process was there to
  take. A row sitting at `queued` for an hour is a message that arrived safely and is still
  waiting.
- Nothing wakes you. There is no resident process and no "deliver to me and I start", so
  timeliness belongs to a hook rather than to the queue: bind `[[hook]]` to `ledger_state` and
  filter the event on `data.to.role.role == "_supervisor"` to learn the moment work lands, and
  let the script carry it to whatever is actually running. The worker set is read at server
  start, so changing `[[hook]]` needs a restart.
- The one thing that can shorten an inbox is a TTL you set yourself. `requeue_ttl_secs` under
  `[server]` defaults to `0`, which expires nothing: a queued receipt for a role with no live
  connection waits indefinitely. Set it positive and `_supervisor` — permanently disconnected —
  has every queued receipt to it eligible for expiry once that TTL passes, so the clock reaches
  your inbox whether or not you meant it to. That one setting is the whole of it: `--ttl` is
  read only on a `--note` send, so a dispatched task carries no deadline of its own, and a row
  that has already been handed out and returned is owned by its own requeue gate rather than by
  the TTL.
- Completion receipts for a task you dispatched always reach you, offline queueing included:
  the ledger's recorded origin is the path, and it needs no standing edge. A role completing
  work it was handed from *another* role is a different path — that one reads
  `allowed_targets`, so keep `_supervisor` in the `allowed_targets` of the roles whose
  receipts you want to collect.

## Operator policy lives outside the core

The events you most need to hear about — a turn that ended without completing, a delivery that
came back `blocked` — are recorded by the client that witnessed them and published onto the
server's stream. What to *do* about them is your policy, and the spec carries it:

```toml
[[hook]]
on = ["delivery_blocked", "turn_end_without_complete"]
run = ["./hooks/notify-supervisor.sh"]
timeout = "10s"
```

- `on` names classes from a closed set: `ledger_state`, `session_state`, `fault`, `role_presence`,
  `gateway_presence`, `spec_reloaded`, `turn_end_without_complete`, `delivery_blocked`, and
  `handoff`. A class outside it refuses the whole load by name, with `spec.toml:<line>`, the same
  treatment a removed key gets.
- The server spawns `run` for each matching event with the event as one JSON object on stdin
  (`seq`, `type`, `data`, `created_at`) and `ONLYNE_SOCKET` set to the admin socket, so the script
  can `onlyne send …` in the same step. A slow script delays nothing: an event reaches every
  subscriber without waiting on any hook worker.
- Delivery is at-least-once per hook. The last handled `seq` is recorded and a restart resumes
  from there, so a script that must not act twice deduplicates on `seq`. A nonzero exit or a
  timeout records fault `hook_failed` once for that event and leaves the event untouched; the hook
  returns to it instead of skipping ahead, so a broken script holds only its own backlog while the
  stream and every other hook carry on.
- The worker set is read at server start: editing `[[hook]]` needs a restart, and a `reload` that
  changes the set names it in the log.

`docs/operations.md` §"Event hooks" carries the full table and a worked script.

## Faults and repair

```bash
onlyne --server-root <root> faults --open-only            # what the core detected
onlyne --server-root <root> repair inspect --task <id>    # read the stored facts
onlyne --server-root <root> repair retry|close --task <id> [--reason <text>]
onlyne --server-root <root> repair fail --task <id> --reason <text>
onlyne --server-root <root> repair adopt --task <id> --backend <name> --reason <text>
onlyne --server-root <root> repair rebind --task <id> --session-id <id> --backend <name> --reason <text>
onlyne --server-root <root> repair ack --fault-id <n> --reason <text>
```

The core detects and records; recovery is your call. `repair retry` handles eligible `queued`
or `in_flight` work; a task whose rows are settled returns `conflict`. `repair fail` settles the
task failed and rejects its undelivered rows, `repair close` settles it cancelled the same way,
`repair adopt` re-points the row's desired backend binding, `repair rebind` moves the row to
another session id and bumps its generation, and `repair inspect` prints the session
projection with every fault recorded against the task. The `kind=task` row is authoritative for
the task outcome. A `kind=completion` row records receipt transport and `out_head`; its state
never reopens a rejected task. Client and server `(generation, seq)` values are per-writer
watermarks, so compare the terminal outcome, lifecycle, resource, and generation rather than
requiring equal sequence numbers. Task control runs beside repair:
`onlyne control recycle|probe|snapshot|cancel|focus --task <id> --from <role> --force
--yes-i-am-supervisor-not-other-role`, where `recycle` and `cancel` carry a required `--reason`
and the other three take none — a `--reason` on `probe` is refused by the parser. `focus` brings
the task's live session to the front of its host. A client holding `max_sessions` sessions pulls with `control_only`, so those
commands reach the session that holds the last slot; work for that role waits until a slot
frees. `DeliveryState::Exhausted` is terminal — retry only after an explicit decision here.
A command on this surface speaks as a role, so `control` and the four message verbs name it with
`--from <role>`; the reads resolve everything from the row they name and take no such flag.
`control` needs no `--to` on this surface: the task's own session row names the role the op has to
reach, so the CLI reads that row and addresses the op there, and an explicit `--to <role>` still
wins. A task no session owns is refused before anything is written, and the refusal names `--to`.

`recycle` and `cancel` end the task on your word: the client asks the plugin for its ending and
closes the host resource, and the plugin's own report settles the task. A word the plugin never
answers is settled by this client after three heartbeat intervals, so a cancel landing before an
agent has written anything still ends its task rather than leaving a row no sweep may take; the
delivery row the client still holds is refused with `operator cancel` or `operator recycle`.

Heartbeat faults carry the liveness verdict, and the row keeps its state through them.
`heartbeat_missing` says the pane's beats stopped and the role link stayed up: the row is
`working`, `onlyne sessions` answers `heartbeat_stale` on it, and the TUI shows `working+stale`.
Check the pane first. A dead process answers `control recycle`, and a live one resumes beating
inside ten seconds and clears the flag on its own. `heartbeat_after_complete` says a session
kept talking after its completion landed; `control recycle` ends the straggler, and the row's own
history keeps the settled completion either way. Both kinds open once per task and stay open
until you `repair ack` them, so the fault table doubles as your to-do list.

`stalled` is the client's own report: a session whose projection tuple froze for
`stall_report_secs` (1800 default, 0 disables) faults once per episode. No-op beats keep the
row beating; `stalled` is the progress verdict beside the liveness one. The verdict means real
silence on live work: the client checks the stored lifecycle at the scan and again at the send
boundary, so a session that already completed has its progress clock retired before any fault
fires. `control probe` first, then `control recycle` or `repair retry` as the answer demands.

`stale_working` covers the owner that left: a row the mirror still reads `working` whose role
has been offline past 600 seconds records this fault once, and the server's own observer
writes it. `[server].stale_watch_secs` (60 default) is the scan cadence for both observers.

A row no client will ever write again is the ghost sweep's work. A mirror row still reading
`working` whose task's own ledger row has already reached a terminal state is rewritten by the
server on its own interval, `[server].ghost_sweep_secs` (60 default, `0` disables the pass).
When the mirror has no outcome, the task ledger supplies it: `acked` settles `done`, `rejected`
and `expired` settle `failed`; an outcome already present in the mirror is preserved. The sequence
advances, a `session_state` event travels, and one row lands in
`ghost_sweeps` naming the session, both sequences, the outcome, and the evidence it acted on
(`task_settled:acked` and the like). `onlyne ghosts [--limit N]` reads that audit, newest first.
One class stays out of its reach: a `working` row whose owner role is offline while the task is
still open, where an ending would decide live work and swallow the requeue that work is owed.
`stale_working` remains that class's only output, and its recovery stays yours through
`repair_*`.

A task-bound unsettled session is retired after either a dropped connection exceeds
`[client] reconnect_grace_secs` (60 default, 0 disables that arm), or an attached transport
accepts no frame for three heartbeat intervals. Both arms settle the task `failed`, refuse the
held delivery with reason `session_dead`, close the host resource, and publish the exit. That
refusal is terminal: create a new task to run the work again. `repair retry` only requeues
eligible queued or in-flight rows and returns `conflict` for a settled task.

A delivery that arrives with the client's accept gate closed is left unanswered: the row
stays `in_flight`, and the next `hello` that does not claim it puts it back on the queue. A
refusal (`accepted: false`) is kept for work this client can never serve, such as an
assignment the plugin declines (`assign rejected`) or a session the operator's word retired with
its delivery still in hand (`operator cancel`, `operator recycle`).

A finished session takes its host resource with it. The client closes the pane, tab, zellij
session, or exec child once that session holds no task and no plugin connection is attached,
and the client log records the closure with `retiring idle session resource`. An idle pane
still open in front of you means the owning client is down.

**A scope that keeps sessions is the one thing that overrides the rule above, and it
overrides it by making the runtime hold on.** `[client.session] scope = "task"` or
`"role"` promises a session that outlives the delivery it served, so that session is not
closed when the work lands — and a `role` pool hands the next delivery to a member that
is still there. The client never reclaims a member on its own: a task-free session is
exempt from the three-heartbeat silence sweep by name, and one whose connection is still
attached is exempt from the reclamation sweep. What ends a member is the runtime leaving,
in one of two ways, both of which reach the same log line above:

- its process exits or its socket ends, which starts `[client] reconnect_grace_secs` (60
  default) and retires the session when that window passes;
- it sends `detach`, which for a session holding no task retires it **at once** — the
  protocol has no other way to read "I am leaving" from a session with nothing left to
  serve, so a graceful goodbye is taken as a final one.

So a `role` pool needs a runtime in one of exactly two shapes. **Resident**: the process
and its socket stay, and it does not `detach` when it runs out of work — that is what
`idle_close = 0` asks for. **Resumable**: the runtime declares `resume`, so a non-zero
`idle_close` suspends it instead, the conversation stays in the runtime's own store, and
the next delivery resumes that conversation rather than starting a new one. `suspend` is
the frame that asks for the release; a runtime declaring neither simply waits for its
agent to leave.

**`pi` reads the scope and acts on it.** Each `assign` carries the role's scope, and
the plugin ends its own process under `oneshot` and stays under `task` or `role` — so a
`role` pool in front of it holds its member and the same conversation takes the next
delivery, while a `oneshot` session leaves and takes its pane with it. That second half
is not a detail: the client retires a session while its agent is still reachable,
because that exemption is what keeps a pool member alive, so a runtime that stayed
under `oneshot` would hold its pane open forever. The leave also has to be the
runtime's own, because the client's teardown is a pane kill and a runtime killed
mid-teardown loses what it had not yet written — pi flushes its session file as it
shuts down.

`pi` declares `register`, `report`, `inject` and `recycle` with **no `resume`**, so the
other door is shut to it: there is no setting that releases its process and brings the
conversation back, and `idle_close` has to stay `0` rather than name a bound. Nothing
in `spec.toml` can assert that half either; it is the agent's to keep.

The tell that a pool is *not* being reused is a `role` role whose `onlyne sessions` shows
a new `session_id` per delivery and none of them left standing. A pool that works looks
like one row reading `lifecycle=idle` with `resource=attached` between deliveries. Before
reading either as a client defect, check whether the agent process is still running —
and read the client log for `the delivery joined the session its scope keeps for it`,
which is the client's own line for a reuse and appears only when one happened.

### Orca sessions

Orca creates one new terminal for each task. `attach` refreshes a persisted terminal handle; it
does not relay into an arbitrary existing session. Spawning requires a running Orca app, an
`orca` CLI that resolves to and reaches that app, and a client launched from the intended Orca
tab with the matching worktree environment (`ORCA_WORKTREE_ID` under the host policy). A
`[single-instance]` CLI error is the immediate spawn refusal. The exact `session_dead` rejection
comes later from the client's retirement sweep after a slot exists.

Automatic re-delivery rides two spec gates: `[server].requeue_max_attempts` (0 unlimited) lands
a returned in-flight row as `rejected` with reason `requeue_exhausted`, and
`[server].requeue_ttl_secs` (0 off) lands returned rows as `expired` with reason `requeue_ttl`.
The TTL also expires never-pulled task, completion, and control rows while their recipient role
has no live connection. `repair retry` always rides outside the gates. A reconnecting client now
also declares its live sessions at `hello`, so a link flap
leaves a running task's row `in_flight` and un-duplicated; a claimed session that dies without
completing gets its row requeued the moment the client reports it exited, and `repair inspect`
keeps the whole trail either way.

A restarted client declares no live sessions, so the server requeues every unacknowledged row
of the roles it reconnects as and the next pull hands them out again; `onlyne ledger --task
<id>` shows the rows that came back.

Both gates leave their mark where you can read it. `onlyne ledger` prints the row it holds, and a
settled row's `reason` travels with it; the key is among the eight the CLI recognizes on a ledger
answer (`msg_id`, `task`, `state`, `reason`, `out_head`, `body`, `family`, `hop_budget` —
`ROW_FIELD_KEYS` in `crates/onlyne-cli/src/ledger.rs`). A row carries the key only where it has a value: a clean `acked` row
simply omits it, and the bytes match what the same row printed before the column existed. The
TUI's page-2 task panel appends `reason=<text>` to the row's tail under the same rule. Six
settlement doors write a value on the column. `requeue_exhausted` and `requeue_ttl` come from the
two gates above. `expired` comes from the deadline sweep on a queued note past its `--ttl`. The
receiving client's refusal carries its own word there: `session_dead` when the reconnect sweep
buries a dead session's delivery, `assign rejected` when the plugin declines an assignment,
`operator cancel` and `operator recycle` when the client settles a session the operator's word
retired while it still held the delivery, whatever `onlyne reject --reason <text>` names, and the
literal `rejected` as the fallback. The operator's own `onlyne repair fail --task <id> --reason
<text>` and `onlyne repair close --task
<id> --reason <text>` write that text onto every undelivered row of the task, the close falling
back to `operator close`. The ack side stays out of it: `onlyne ack --msg-id <id> --reason <text>
--force --yes-i-am-supervisor-not-other-role`
settles the row through `mark_acked` and keeps a reason the row already carried, and
`onlyne repair ack --fault-id <n> --reason <text>` writes its reason on the fault row alone. Both
run against a role workspace or client socket, and both require `--reason`.

## Errors you will see

`acl_denied` → the edge is missing from the spec: the sender's `allowed_targets` and
the receiver's `allowed_senders` have to name each other. Your own sends read
`allowed_targets` alone, so a denial on one means that list is non-empty and omits
the target. `unauthorized` → the handshake refused the peer: an unregistered key, an
unregistered role, a key registered for another role, or a bad signature.
`recipient_offline` → a `note` found nothing to wake: its role was offline, or online
with no session running and `note_queue` off. The message says which.
`duplicate` → the same `op_id` again; its `data` is the original receipt, byte for
byte. `conflict` → same `op_id`, different body. `not_admin` → a `cluster:` principal
sent without an admin standing; a role that is neither the administrator nor the task
owner gets `forbidden` on a control frame. Every reject writes no ledger row and
leaves no sender intent. A queued note's `--ttl` deadline sits on its ledger row, and
the server re-arms every stored deadline at startup, so the sweep answers `expired`
after a restart too. A note that was already queued when the server was upgraded
carries no deadline and stays `queued`; settle it with `repair fail` or
`repair close`.

## Watching with the TUI

`onlyne tui --server-root <root>` opens the three-page board in this process, reading the
admin surface: `1`/`2`/`3` switch pages, `Tab` walks a page's panes, `↑`/`↓` step the
selected row, `PgUp`/`PgDn` (or `[`/`]`) scroll a pane of lines, and `q` or `Esc` leaves.
The cluster page lists every role with its presence, the sessions it holds (`sess`), how
many of them are busy, idle or suspended, and its queue depth; beside it is the selected
role's board, one row per delivery in the five-column reading (`queued`, `running`,
`waiting`, `done`, `failed_or_blocked`), and under both is the event tail. `Enter` opens the
selected card's family on the task page, which is that family's path across roles — every
delivery by hop, with its verdict and the receipt it settled with — over the tail of the
session serving the selected delivery. The faults page lists the open faults, the selected
fault's own fields, and the repair verbs it offers with the key that opens each: `a`
`repair ack`, `t` `repair retry`, `c` `repair close`, `F` `repair fail`, `i`
`repair inspect`.

The four operations are forms the board opens on the selection: `s` sends a task (`from`,
`to`, `body`), `f` focuses (`from`, `to`, `task` — the target is named by hand, not read off
the selection), `r` reports a task's verdict (`from`, `task`, `outcome`, `head`), and the
repair keys above. `Enter` submits a form, `Esc` cancels it, `Tab` moves the caret between
its fields, and the footer's second line prints what the op answered. `^R` re-reads the
snapshot. Nothing polls: a page moves when the admin stream says the cluster did, and a gap
in that stream leaves the header saying `catching up` while the snapshot is re-read.

`onlyne tui --server-root <root> --once` renders one frame — the cluster page, from one
snapshot — as plain text and exits.

## Clusters under clusters

Your own client connects to a parent server as a plain `[[client]]` entry marked
`aggregate = "<child-cluster>"`. The label is an annotation: it contributes no ACL rows,
the delivery path carries no aggregate branch, and a generated `[[client]]` fragment
drops it. Tasks flow in, completions flow out, and every parent ledger row is written from
the parent-side envelope, so its `from`, `to`, and body name parent-visible roles alone.
Hand the parent operator your interface text with `onlyne cluster export-prose --role
<role>`, which prints the prose raw for pasting into a TOML multi-line string and wraps it
in an object under `--json`. The protocol contains zero federation code, so there is
nothing to break.
