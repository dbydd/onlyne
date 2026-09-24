# Operations

**English**

Onlyne operations are bounded by the server ledger, the client workspace, and the admin local socket (canonical name `.onlyne/run/s`; bind to this path when it is no more than 103 bytes, and to a short derived path under the system temporary directory when the limit is exceeded; record the actually served path in the `run/socket` marker in that same directory).

## Duty and operations entry points

`onlyne status` reads server status through the admin local socket.

`onlyne roles` reads the role registry and online state.

`onlyne sessions` reads the session projection. The answer is the mirror row written by the heartbeat, and `updated_at` is its age.

`onlyne sessions --fresh --task <task>` obtains ground truth on the spot: the server sends the existing `control` op `probe` to the client that owns the task, waits for that row to advance past the `(generation, seq)` from the start of the read, and then answers from that row. The admin vocabulary gains no verb, and the client side gains no code.

The upper bound for the wait is this read's own `--timeout` minus the 250ms reserved for frame round trips. If the probe does not land, the read answers from the stored mirror as usual; it neither exceeds this bound nor relaxes the bound to wait longer.

The answer to `--fresh` carries a `fresh` field on the row: `probed` is the observation after the probe lands, `offline` means there is no object to ask (no `--task` was given, the task has no row, no client owns it, or the owner is offline), and `unanswered` means the probe went out but nothing was republished within the bound. All three cases answer with that row; they do not report an error or hang.

`--fresh` must include `--task`: a fresh read asks the client for a named task.

A read without `--fresh` is byte-for-byte identical to the previous behavior: it sends no control frame, performs no wait, and its answer has no `fresh` key.

`onlyne ledger` reads the delivery ledger.

`onlyne faults` reads the faults table.

`onlyne watch` reads the durable and advisory event streams.

`onlyne history` replays the event log.

`onlyne spec_diff` compares the running spec with the spec on disk.

`onlyne tui --server-root <root> --once --page <1|2> --state <active|all>` renders one plain-text frame and exits. `active` is the default filter and keeps only the active view; `all` also includes settled sessions and ledger rows. Page 2 with `--state all` shows the `reason=<text>` of settled rows directly.

## Reading the service path

When each daemon binds its socket, it publishes the actually served path at `<owner>/.onlyne/run/socket` (mode `0600`, one absolute path followed by a newline). An operator can read this path in three ways:

`onlyne-client status --workspace <dir>` prints `socket <path>`; the field value is the served path.

The client log names this path at startup: in the short-path case, one line gives the canonical path, its byte length, the served path, and the marker (`adapter socket moved to the short path`); in the canonical-path case, one line gives the socket path (`adapter socket serving`). The server follows the same convention: in the short-path case, one line gives the served and canonical paths and their lengths (`the run socket is served from a short path; the marker names it`), while the normal case logs `the run socket is open`.

`cat <workspace>/.onlyne/run/socket` reads the marker file directly.

A session process starts with `ONLYNE_SOCKET` set to this served path; the `onlyne` command in a role pane uses it to reach the socket directly. The CLI resolution order is `--socket` > `ONLYNE_SOCKET` > `--server-root` > upward lookup from `--workspace`/cwd. The lookup recognizes the owner directory by `.onlyne/run/s` or `.onlyne/run/socket`, and resolves the path through `socket_path()`.

A client whose bind fails exits with code 1 and writes one stderr line, `onlyne-client: bind the workspace socket <规范路径>: <明细>`; the detail gives the served path, the byte length of each path, and the OS reason. A client that cannot bind its socket chooses to exit; the loop that kept the TLS link alive for silent retries has been removed. An `accept` error after a successful bind is logged at `error` level (`adapter socket accept failed; retrying`), retried every 100 milliseconds, with the listener retained.

The verification chain is pinned by `crates/onlyne-testkit/e2e/socket-path-length.sh`: a deeply padded workspace, a short served path, marker publication, the canonical path remaining unused, and end-to-end task completion.

## Configuration loading

Unrecognized keys in spec.toml or a role workspace's config.toml are ignored and the process starts normally; each ignored key produces one `tracing` warning line in the daemon log when it is loaded. An invalid value for a real key remains a hard error that prevents startup. Consequently, a misspelled key silently falls back to its default, with no signal other than that warning line.

After a version upgrade, a role client and its server must run the same build: if the `hello` reply is missing a field, the two cannot connect. During an upgrade, restart the server and every `onlyne-client` together.

## Concurrency

The knob for running multiple sessions concurrently for the same role is `[[client]].max_sessions`.

The meaning of `max_sessions` is the maximum number of sessions in flight at the same time.

Each task has its own session.

After a role reaches `max_sessions`, it stops pulling new tasks.

A full-capacity role replaces `pull` with `control_only = true` and keeps receiving.

Control rows and task rows share one pull queue, but the capacity gate stops tasks.

`recycle`, `cancel`, and `focus` are commands used to free capacity and inspect the current state, and they must arrive exactly when the role is full. Therefore, the server delivers only control rows on this path; the task row's `queued` state and ticket remain unchanged.

The server retains the pending accounting entry and offers it again on the next pull.

The seed value for `onlyne-client init` is `max_sessions = 1`.

The seed value protects a single-pane manual environment.

Set concurrency explicitly for each role in the spec.

After changing the spec, run `onlyne reload` to make it effective.

An existing client automatically refreshes its role slice after receiving a `SpecReloaded` event.

After an existing client refreshes its role slice, it immediately uses the new `max_sessions` gate.

An existing client does not need to restart.

`crates/onlyne-testkit/e2e/reconnect-requeue.sh` covers the pending-then-offer-again path with `max_sessions = 2` and three tasks.

Control still reaches the role at full capacity. `a_control_only_pull_hands_the_command_and_leaves_the_work_queued` in `crates/onlyne-server/tests/delivery.rs` pins this at the protocol level, and step d of `crates/onlyne-testkit/e2e/herdr-live.sh` verifies it once on a live host: the case's role uses the seed value `max_sessions = 1`, its only slot is occupied by a `sleep`, and `control focus` still reaches the session's pane.

## Focus

`onlyne control --from <role> focus --task <id> --force --yes-i-am-supervisor-not-other-role` brings a session's pane to the foreground. The TUI entry point is `F`, and it acts on the selected row.

The control plane uses `ControlOp::Focus{task_id}`, with ledger row `kind = control`. The command reaches the session's `backend_ref`. The herdr backend follows a three-stage chain: `herdr workspace focus <W>`, `herdr tab focus <T>`, and then a third stage that branches according to the pane's origin. A managed agent uses `herdr agent focus <pane_id>`; a shell pane launched by `herdr pane run` uses `herdr pane focus --pane <base_pane> --direction <split_direction>`. Those two values were recorded when the pane was split. `base_pane` and `split_direction` are stored in `backend_ref`, so the anchors live with the pane.

`herdr pane get <pane_id>` confirms the final step. Delivery succeeds only when `result.pane.focused` is true. If focus lands elsewhere, the command reports an error and names the pane that currently holds focus. A `focus()` failure records a `Report::Fault{kind:"focus"}`, and the TUI prints the backend's original text in that row's feedback field.

`--from` is a global flag on the admin plane and is written after `control`. The ACL for the focus command follows the same rule as delivery: a role with a `send` edge to the target controls the target session, while `ControlOp::Broadcast` requires a global edge.

The herdr backend recognizes a workspace by label (`onlyne:<cluster>`) and a tab by name (the role's own name). To place a session in the workspace and role tab currently at hand, rename them before launching the session: `herdr workspace rename <WORKSPACE_ID> onlyne:<cluster>` and `herdr tab rename <TAB_ID> <role>`. A workspace with a mismatched label receives a second workspace, and a mismatched tab name receives a second tab. The client then logs a warning naming the label and the newly created workspace.

## Fault recovery

A fault is an auditable fact recorded by the server.

A fault enters the `faults` table.

A fault is pushed to observers through advisory `Event::Fault`.

`onlyne repair inspect --task <id>` reads the recovery context for a task.

`onlyne repair adopt --task <id> --backend <backend> [--backend-ref <值>] --reason <reason>` replaces the backend binding in that row's desired state, while leaving the row's session id and generation unchanged and advancing seq by one. To move a task to another session, use `rebind` below.

`onlyne repair rebind --task <id> --session-id <session> --backend <backend> [--backend-ref <值>] --reason <reason>` rewrites the task's backend binding, replaces the row's session id with the supplied value, increments generation, resets seq to zero, and causes reports for the old generation to be ignored from then on.

Both verbs follow the same rule for `--backend-ref`: a value that parses entirely as JSON goes live as the parsed result (so an object-shaped value such as a pane reference can be written directly as `--backend-ref '{"id":"p-7"}'`); all other text goes live as a JSON string; and an omitted flag goes live as null. The server and client consume the value as an object (`crates/onlyne-server/src/faults.rs:253-257,273-277`, `crates/onlyne-client/src/session/dispatch.rs`).

`onlyne repair retry --task <id> --reason <reason>` sends a retryable task back to the queue.

`onlyne repair fail --task <id> --reason <reason>` converges the task to failed.

`onlyne repair close --task <id> --reason <reason>` closes recovery work.

`onlyne repair ack --fault-id <fault-id> --reason <reason>` acknowledges a fault.

The repair family uses the admin plane at `<server-root>/.onlyne/run/s`; when the tree exceeds the 103-byte limit, it uses the short served path recorded in `run/socket`.

The repair family does not pass through the role workspace's adapter socket.

## Delivery and requeueing

Onlyne's SQLite databases must be placed on a local filesystem. Do not put the server root or role workspace in OneDrive, Dropbox, iCloud, a network drive, or any other synchronized directory. SQLite depends on WAL files and local file locks; Onlyne does not run `quick_check` or `integrity_check` at startup, so a corrupt `state.db` / `client.db` makes the daemon fail on its first database operation. First stop every client/server, then copy the entire root unchanged to a local directory and inspect the databases on the copy; do not run checkpoint, VACUUM, or repair on the original database.

When a role link dies, the server requeues that role's `in_flight` delivery rows to `queued`, where they wait for the next pull before delivery.

Takeover requeueing when a new link lands follows the same path. The `live_tasks` field of `hello` declares the session tasks still alive in that client's memory; declared rows remain `in_flight`, and their delivery tickets are reattached to the new link's generation, so they are requeued normally if that link later terminates.

If a declared session dies before completion, the client publishes an `exited` projection. When the server sees an `in_flight` row with the same `session_id` as that session ticket, it requeues the row, again through the gates below.

Automatic requeueing is governed by two budget knobs; manual `onlyne repair retry` does not pass through the gates. An `in_flight` row returned to `queued` is evaluated first by `requeue_ttl_secs`, then by `requeue_max_attempts`. A task, completion, or control row for a receiving role with no live connection that has never been pulled expires after the same TTL when `requeue_ttl_secs` is nonzero; with the default value of 0, it remains queued.

| Configuration file | Field | Default | Effect |
|---|---|---|---|
| `[server]` in `<server-root>/.onlyne/spec.toml` | `requeue_max_attempts` | 0 | Maximum automatic requeue attempts allowed for a row; 0 means unlimited. A row that exceeds the limit becomes `rejected` with reason `requeue_exhausted` |
| `[server]` in `<server-root>/.onlyne/spec.toml` | `requeue_ttl_secs` | 0 | TTL allowed for rows returned to the queue and unpulled rows whose receiving role has no live connection, measured from enqueue time; 0 disables it. An over-age row becomes `expired` with reason `requeue_ttl` |

Evaluate TTL first, then attempt count; each produces one `ledger_state` event.

How to read `reason`: the row keys printed by `onlyne ledger` are `msg_id`, `task`, `state`, `reason`, `out_head`, `body`, `family`, and `hop_budget`. A key appears only when that row has a value; rows and columns without values remain byte-for-byte identical to before. The task detail panel on TUI page 2 appends `reason=<text>` to the end of the ledger row. Values that enter this column include `requeue_exhausted` and `requeue_ttl` from the two gates above; `expired` from the expiration sweep; `session_dead` from the rejection written when a client settles a disconnected session (see “Session ghosts and ownership determination”); `operator cancel` and `operator recycle` from a client settling by itself and rejecting the delivery row it still holds when no one answers the operator's word (see the control section below); and the full rejection text when a pane backend (`herdr` / `orca` / `zellij`) rejects a protocol `session_command` before opening the page (see the final paragraph of “Headless (`exec`) sessions”). Text entered by an operator through `onlyne reject --reason` or `onlyne repair fail --reason` enters this column unchanged. When `onlyne ack` accepts it, that text travels with the settlement event and the row's `reason` remains unchanged. The string `operator ack` is test data in the faults table's `reason` column (the `update_fault_state` case in `crates/onlyne-store/src/tests.rs`); the ledger column has no record of it.

Both the push-delivery and pull-delivery transitions of `in_flight` emit a `ledger_state` event; at every sampling point, ledger state read offline and the session projection agree with each other.

The complete chain (the server dies with a live link, the client reconnects and takes over, and a single session completes normally) is verified against real processes by `crates/onlyne-testkit/e2e/requeue-claim.sh`.

When a task that this role has already completed as `Done` is delivered again, the client recognizes it. The decision reads the session terminal state from `client.db`, with the entry point `DispatchState::task_completed_here` in `onlyne-client`, and acts before the capacity gate in `accept_delivery`: it acknowledges the row in place, sets `accepted = true`, uses reason `task already completed by this role`, does not stage a session, and consumes no capacity. That reason enters the `ledger_state` event, and the row itself becomes `acked`. At the same time, the log records `redelivery of a finished task settled without running it`, with `msg_id` and `task`.

This decision reads `Done`. A task whose session was terminated, crashed, or was recorded locally as `Failed` can be requeued by `onlyne repair retry` only while it still has eligible `queued` / `in_flight` delivery rows. To rerun a task already rejected by a terminal state such as `session_dead`, the operator sends a new task with the guarded `send` verb. A row already `Done` waits for that acknowledgment and an empty run.

## Rejection surface

`onlyne ack --msg-id <id> --reason <text> --force --yes-i-am-supervisor-not-other-role` settles a delivery as `acked`.

`onlyne reject --msg-id <id> --reason <text> --force --yes-i-am-supervisor-not-other-role` settles a delivery as `rejected`.

`--reason` is required for both verbs.

`--op-id` is optional for both verbs.

The rejection reason is stored on the ledger row.

Both verbs use the role workspace's adapter socket.

`--request` is rejected by both verbs.

When a plugin returns `accepted = false` for an `assign`, the client queues a rejection acknowledgment with the same `msg_id`.

That rejection acknowledgment shares the durable intent queue with completion and is sent after reconnection if the connection drops.

The rejection reason comes from the plugin; when the plugin supplies none, record `assign rejected`.

When an already settled delivery receives another acknowledgment or rejection, the server returns the same state event.

## Session ghosts and ownership determination

The lifecycle owner is the role's own client process.

While the client is dead, nobody determines session lifecycle on behalf of that role.

While the owner process is alive, the client settles a disconnected session: after the plugin connection drops and `reconnect_grace_secs` expires, it retires the session it left behind.

Settlement writes two records: the task becomes `failed`, and the delivery row it holds is rejected with `session_dead`. The same retirement pass then sends that session's own projection with a heartbeat report, so the server row immediately reads `exited`, without waiting for the observer's `stale_working` or `heartbeat_missing`.

That rejection is terminal. To perform the work again, create a new task. `repair retry` handles only eligible `queued` / `in_flight` rows and returns `conflict` when the task row is already settled.

Ledger decisions use `kind=task` rows. A `kind=completion` row records receipt transport and `out_head` and does not reopen a task already `rejected`. The projection's `(generation, seq)` is each writer's own watermark; the client and server do not require them to match. Terminal-state reads compare task outcome, lifecycle, resource, and generation.

A restarted client does not make terminal-state decisions for old ledger entries: an `Acked` row is one it answered itself, so the restart does not report its death or decide for another owner.

Ghost detection and stall reporting have five knobs:

| Configuration file | Field | Default | Effect |
|---|---|---|---|
| `<workspace>/.onlyne/config.toml` | `stall_report_secs` | 1800 | Maximum duration for a frozen session projection tuple, in seconds, after which the client reports a `stalled` fault; 0 disables it |
| `<workspace>/.onlyne/config.toml` | `reconnect_grace_secs` | 60 | Time in seconds allowed for a plugin connection to be absent after disconnect; after expiry the client retires the session it left behind and settles what that session owed. A session still bound to a task makes that task `failed` and rejects its delivery row with `session_dead`; 0 disables it |
| `[server]` in `<server-root>/.onlyne/spec.toml` | `stale_watch_secs` | 60 | Server observer scan interval in seconds; 0 disables the observer |
| `[server]` in `<server-root>/.onlyne/spec.toml` | `heartbeat_grace_secs` | 90 | Heartbeat silence allowed for a `working` row when its owner is online, in seconds |
| `[server]` in `<server-root>/.onlyne/spec.toml` | `ghost_sweep_secs` | 60 | Server ghost sweep scan interval in seconds; 0 disables the sweep |

The server-side observer scans at `[server].stale_watch_secs` intervals, running two detectors on each scan.

Detector one scans `working` rows whose owner is offline.

The offline detector records a fault with kind `stale_working` after 600 seconds.

Detector two scans `working` rows whose owner is online, using heartbeat freshness as its criterion.

The pi plugin sends a heartbeat packet every 10 seconds. Each time the client receives a beat, it resends the complete projection in its own heartbeat report, and the server row's `updated_at` advances with the heartbeat.

When you need an immediate observation without waiting for the next heartbeat, use `onlyne sessions --fresh --task <task>`: it sends `probe` to the owner client, waits for the row to advance past the watermark from the start of the read, and the plugin answers that `probe` with a heartbeat.

A no-op heartbeat that advances a silent beat also carries a liveness fact. The client increments its local version and resends it, keeping `updated_at` fresh for a healthy session.

When the owner is online, the row age exceeds `[server].heartbeat_grace_secs` (default 90 seconds), and this process has seen a write to that row, detector two records a fault with kind `heartbeat_missing`.

A `heartbeat_missing` row also appears in the `heartbeat_stale` field of the `sessions` answer, and the TUI presents it as `working+stale`.

When the same task already has an unacknowledged fault of the same kind, detector two does not record another one.

At server startup, every row that is `working` at that moment is registered as seen, including silent rows left by the previous process.

When a heartbeat in the same generation raises a row from its settled `exited` completion back to `working`, the observer records a fault with kind `heartbeat_after_complete` and applies the projection normally.

Both detectors only record faults and push advisory `Event::Fault`.

Separate from the observer, a ghost sweep runs at `[server].ghost_sweep_secs` intervals, with its first tick at half an interval.

It reads `working` rows from the mirror and aligns each row with that task's own ledger row.

A `working` mirror whose ledger row has reached a terminal state is a fossil: the ledger has settled, but the mirror still holds the old bytes.

A sweep uses the task ledger decision only when the mirror has no outcome: `acked` reads as `done`, while `rejected` and `expired` read as `failed`. An outcome already published by the mirror remains unchanged.

Writes use the same settlement path shared by the `repair` family: rewrite `observed_json`, increment `seq`, persist, and push durable `session_state`.

When the ledger row is still `queued` or `in_flight`, the sweep leaves it unchanged.

Every moved row produces one row in the `ghost_sweeps` audit table. Every field in that row comes from the row the server already holds: task, role, session, generation, `seq_before`, `seq_after`, outcome, evidence, and `swept_at`.

The `evidence` text is a label plus the ledger state that proves this write, for example `task_settled:acked`.

`onlyne ghosts [--limit N]` reads this table with the newest sweep first. It is an admin-plane read and is rejected on the client plane.

The client reports `stalled` through the role's `report` plane: if the session projection tuple has no `Applied` change for more than `stall_report_secs`, the client sends a `Report::Fault` with kind `stalled`, including the task and session identities.

A no-op heartbeat advances the liveness watermark, not the progress watermark; `stalled` watches only the latter.

The reason text for `stalled` is `no applied progress`.

A settled task cannot enter this criterion: only task assignment establishes the progress clock, and `note_applied` only refreshes an established clock. The final `agent: "idle"` heartbeat sent by the plugin after its completion receipt lands on nothing. Expiration scans and send boundaries each derive lifecycle once—the stored tuple and the task's settled value in the `task` table pass through `project` together—and a task that resolves to `exited` is suppressed and forgotten; releasing the connection also forgets the clock for every session it served.

The same frozen episode is reported only once, and the next `Applied` clears deduplication. `stall_report_secs = 0` disables this criterion.

The row does not transition: `stalled` only enters the faults table and pushes an event; recovery decisions remain with the supervisor and repair family. Only the ghost sweep moves projections, under the rule at the end of this section.

Disconnection grace uses another knob. When a plugin connection ends without a `detach` frame, the client retains its session and host resources, and `reconnect_grace_secs` starts at that moment.

A reconnection within the window clears this clock: the agent returns to its original session and receives its next task normally.

If it has not returned after the window expires, the client retires it: it closes the host resources and releases the capacity slot.

Before retirement, first settle what the session owed: if it is still bound to a task, that task becomes `failed` and the delivery row it holds is rejected with `session_dead`. The same retirement pass then sends that session's own projection, so the server row immediately reads `exited`. A slot with no bound task is only retired.

The retirement reason is the terminal state already recorded for the task under that session; when no terminal state is available, record `Fault`. `reconnect_grace_secs = 0` disables the disconnection-grace criterion.

Disconnection grace and attached-but-silent are two retirement paths. A disconnected connection uses `reconnect_grace_secs`; a connection that remains present is also retired when its bound unsettled task has no accepted frame for three consecutive heartbeat intervals. Both paths settle the task as `failed`, reject the held delivery with `session_dead`, close resources, and publish exit.

When a newer session is already serving the same task, a connection returning with the old id is downgraded to read-only: it no longer receives `assign`, `deliver`, or `render_send`, and it does not occupy that session's delivery plane.

The read-only connection is still admitted. The `send` frames it sends do not enter the durable queue or go to the server; they accumulate in that task's buffer until one merge.

The downgrade lasts only while that newer connection is online. When it ends, the first connection accumulated for that session is promoted to its transport and its read-only flag is cleared, so a plugin that reattaches before the client notices that the old socket is dead is not permanently silenced.

When the retrying session completes, its buffer and its own handoff are merged by downstream role into one entry: one envelope per role, with every body line labeled by source. `[retry]` was written by the completing session, and `[zombie]` was written by the accumulated old connection.

After merged delivery, the read-only connection receives `bye` and is removed. If it still holds its own slot, that slot is retired as `Replaced`. The completion is settled only once; this step neither settles nor releases again.

There is one exception: if the report that caused this merge arrived on this read-only connection, it receives no `bye` during this round. The client first writes the response to that report, and connection teardown is left to its own `detach` frame or socket closure. The plugin handles `bye` by disconnecting the socket and marking every in-flight request as failed. A `bye` that arrives before the response makes an already recorded completion read as failed, so the agent resends its terminal state. `a_read_only_completion_is_answered_before_any_bye` in `crates/onlyne-client/tests/scenarios/reconnect.rs` pins the order.

The kind field in the faults table stores the text `stale_working`, `heartbeat_missing`, `heartbeat_after_complete`, and `stalled`.

The server-side observer does not change ledger state.

The server-side observer does not trigger retry.

The server-side observer does not trigger fail.

The server-side observer obeys the zero-policy red line in §8.

Within that red line, the ghost sweep moves one class of row: rows whose mirror still reads `working` while that task's own ledger row has reached a terminal state.

The decision written to the mirror comes from that ledger row. The same pass also settles the task's still-unsettled delivery rows, except `kind = completion` receipts: a receipt records this settlement itself, its result is written in its `out_head`, and rejecting it would erase the result from the ledger. The recipient's client acknowledges it when it returns; a role without a client retains it under the “receipts stay queued” rule (see “Rejection surface”).

The class of row eligible for this sweep depends on the backend. The server synthesizes **bare heartbeats without a projection** as `working` / `running` / `attached` (`crates/onlyne-server/src/projection.rs`), so only a backend that continues sending bare heartbeats after settlement can slide the mirror from `exited` back to `working`. Observation: pi + orca sends no bare heartbeat after task settlement. The completion path itself publishes once (lifecycle moves from `Done` + `Accepted` to `exited`), then retirement publishes again (`agent` becomes `gone` and the resource becomes `closed`), with no third frame between them. An agent process for a long-lived backend such as acp can outlive the session, and only then does this shape occur.

It does not touch another class of `working` row: one whose owner is offline while the task is still unsettled.

Writing a decision for such a row would assign a terminal outcome to work that is still alive and would swallow the requeue it deserves.

`stale_working` remains the only output for such a row, and recovery decisions still belong to the supervisor and the `repair_*` family.

The source of truth for liveness is the pi-onlyne heartbeat packet itself; pane and host-terminal liveness are outside the server's decision plane.

Recovery decisions belong to people and supervisor roles.

People use the repair family to perform `inspect`, `adopt`, `rebind`, `retry`, `fail`, `close`, and `ack`.

Supervisor roles use the control verb to perform recovery actions.

The reason is required for both `onlyne control --task <id> recycle --reason <text> --force --yes-i-am-supervisor-not-other-role` and `onlyne control --task <id> cancel --reason <text> --force --yes-i-am-supervisor-not-other-role`.

`onlyne control` on the admin plane does not require `--to`: by default the CLI first reads the task's session row and sends control to the owning role. When `--to <role>` is supplied explicitly, it is used directly without another frame read.

When no session for a task belongs to any role, the command rejects before writing anything, exits with code 4, and writes this exact stderr text: `onlyne: no session owns task <id>; pass --to <role> to say where the control goes`.

`recycle` and `cancel` settle a task with the operator's word: the client asks the plugin to finish and close the host resources, and settlement occurs when the plugin report lands. If the plugin never answers, after three heartbeat intervals (`CONTROL_SETTLE_BOUND`) the client settles by itself using the operator's word, records `cancelled` for `cancel` and `failed` for `recycle`, and rejects the delivery row it still holds with `operator cancel` / `operator recycle`.

Timeout settlement writes only the first decision: when another gate has already settled the task, it writes and publishes nothing; when the store rejects the write, the word is recorded again at the original time and retried on the next beat. If another gate has already settled the task while the mirror is still nonterminal, the later ghost sweep moves that old mirror according to the task's terminal ledger state.

The supervisor role's control verbs require spec authorization.

If the spec declares no role with `admin = true`, the admin identity does not exist.

Control by an admin identity bypasses role-edge-table checks.

`onlyne control` on the admin socket executes as the admin identity.

`control` for a non-admin role still requires the owner identity or an edge with `admin = true`.

When roles have no control authorization, a proxy-hop `control cancel` returns `forbidden`.

`control cancel` returning `forbidden` is by design.

In that case, the recovery entry point is the repair family on the admin local socket.

```bash
onlyne --server-root <server-root> repair inspect --task <id>
onlyne --server-root <server-root> repair fail --task <id> --reason session_dead
```

The first command reads the residual ledger and session projection.

The second command converges the task to failed while preserving the `session_dead` reason.

## supervisor maintenance command set

The seven verbs `send`, `reply`, `handoff`, `complete`, `ack`, `reject`, and `control` require both `--force` and `--yes-i-am-supervisor-not-other-role`.

If either flag is missing, the command exits 2 before resolving the socket.

The rejection text names the plugin tool the role should use in a session: `send` uses `onlyne_send`, `handoff` uses `onlyne_handoff`, and `complete` uses `onlyne_complete`; the plugin answers for the other four verbs itself.

The CLI gate belongs to supervisors and to `exec` roles whose sessions have no plugin.

An `exec` role uses the CLI form and identifies itself with those two flags.

The read verbs `repair *`, `ledger`, `sessions`, `roles`, `faults`, `watch`, `history`, and `status`, plus `reload` and `shutdown`, do not carry those two flags.

## Task families and metadata

A task family carries its own metadata, written at its origin through the guarded `send` verb:

```bash
onlyne send --hop-budget <n> --label <k=v> --deadline <rfc3339> \
  --force --yes-i-am-supervisor-not-other-role
```

`--hop-budget <n>` records how many hops the family may spend, `--label <k=v>` records arbitrary key/value pairs for scripts to read and may be repeated up to 8 times, and `--deadline <rfc3339>` records the family's wall-clock deadline.

`family` is the id of the family's root task; it passes unchanged through every hop. A child task inherits `hop_budget`, `origin` (the role that started the root task), `deadline`, and all `labels`, while `hop` is the parent row plus one.

Inheritance occurs in exactly one place, `Causality::child_of`: both the CLI's `handoff` and the plugin's `onlyne_handoff` use it, so both entry points produce the same chain shape.

`onlyne ledger` prints two additional keys, `family` and `hop_budget`; rows without values do not contain those keys, and old rows and columns remain byte-for-byte identical to before.

`labels` is the only core field the system does not interpret: at most 8 entries, keys no longer than 32 bytes, and values no longer than 256 bytes. `Envelope::validate` rejects an out-of-bounds value and names the field.

New ledger-table columns are added in place, like `expires_at` and `requeued`, so the server's schema marker remains 4 and an existing state.db need not be rebuilt.

## Host resource reclamation

When a session ends, its host resources are reclaimed: the client closes the herdr pane, Orca tab, zellij session, or exec child process when the session holds no task and has no plugin transport attached. After settlement, an empty shell left in a role tab is removed by these three paths; manual `herdr pane close` is the fallback.

There are three trigger paths:

- Graceful plugin `detach`: close the resources of every idle session served by that connection in place.
- Settled with no agent attached: closure happens at the moment of settlement.
- 250 ms readiness tick: scan tracked sessions. Whether a session has ended is derived: the stored tuple and the task's settled value in the `task` table pass through `project` together, and the session is closed only when the result is `exited`. The reason is derived from that settled value in the task table (`done` gives `Completed`, `failed` gives `Fault`, and `cancelled` gives `Cancelled`; `pending` or no record produces no reason). The session row itself no longer stores lifecycle and no longer reports its task result.

After settlement, a session accepts no new task: one task uses one session, the slot is returned immediately and no longer occupies `max_sessions`, and host resources are reclaimed through the three paths above. There is one retention path: if the connection drops without a plugin `detach`, that agent may still reconnect.

Each reclamation first refreshes a stale ref through `backend.attach` while the stored resource state remains open, projects `resource_closed`, and writes one `retiring idle session resource` line in the client log with the fields `task`, `backend`, `resource`, and `reason`; the slot is then removed from the tracking table. A close failure records a warning, and the run continues normally.

Closure in herdr is idempotent: a `pane_not_found` response to `herdr pane close` is recorded as success, and the log records one debug line, `herdr pane already closed`, with the fields `task` and `pane`. A workspace that has already disappeared reads as closed afterward.

The meaning of `stalled` therefore narrows to true silence: a `stalled` with `no applied progress` for a completed task disappears from this surface, and `stalled` in the fault table now describes only sessions that are still running. See the progress-clock entry in the previous section for the criterion.

### Orca sessions

Each task creates a new Orca terminal. `attach` only refreshes the saved terminal handle; it does not forward messages to any existing session. Before startup, check that `orca status --json` points to the expected running Orca app, verify the `ORCA_WORKTREE_ID` inherited by the client from the expected Orca tab, and confirm that `orca` resolves to the expected CLI and can connect to that app. A CLI return of `[single-instance]` means terminal creation is refused at that moment. `session_dead` appears only later, during the client's retirement sweep, after the slot exists. Once delivery has entered a terminal state because of `session_dead`, a new task must be created to run the work after repairing the host.

## Headless (`exec`) sessions

`exec` is the canonical name of the headless backend; `headless` is only a parse alias, while the backend string in projections and events remains `exec`. The selection chain is env `ONLYNE_BACKEND` (nonempty) > `backend` in the workspace's `config.toml` > auto. `exec` / `acp` / `fake` are not selected by host detection and must be enabled by name; `headless` behaves the same way as `exec`.

The session child's stdout/stderr is merged into `<workspace>/.onlyne/logs/session-<task>.log`. When the process exits, the held `probe` writes at most the last 200 lines of that file (truncated to approximately 16KiB first, then split on whole lines) into `ResourceProbe.detail.output_tail`; if the log is missing or cannot be read, the key is omitted while the `exit` code remains.

The closure ladder is:

- unix: the session is an independent process group. `close` uses `kill(2)` to signal only the recorded pgid (`backend_ref.pgid`, identical to the leader pid), sends `SIGTERM` first, waits 5 seconds, then sends `SIGKILL`, and finally uses `child.kill` to reap the process. If signaling the group fails, it falls back to the same signal for the leader pid. It refuses to send to pid 0/`-1` (those mean “this process group / every killable process,” not the session). It never kills processes by a cmdline wildcard.
- windows: spawn uses `CREATE_NEW_PROCESS_GROUP`; shutdown first sends `GenerateConsoleCtrlEvent(CTRL_BREAK)`, waits for the grace period, then calls `child.kill()` (TerminateProcess). CTRL_BREAK fails when the client has no console, so termination proceeds directly. Windows has no SIGTERM; the supervisor performs shutdown by running `onlyne server stop` on the host containing the server root.

`pi --mode rpc` is a typical `session_command` for this backend: the client holds stdin open (EOF means operator departure for rpc), stdout goes to the session log, and the message plane uses the adapter socket rather than the child's stdio.

```toml
# <workspace>/.onlyne/config.toml
backend = "headless"

# <server-root>/.onlyne/spec.toml [[client]]
session_command = ["pi", "--mode", "rpc", "--session-id", "{session}"]
```

Protocol-oriented `session_command` values (commands such as `pi --mode rpc` or `agent --acp` that speak JSON-RPC over their own stdio) recognize only the `backend = "exec"` and `backend = "acp"` configurations. When written as `herdr` / `orca` / `zellij`, the client rejects them at delivery and stores the rejection text as the reason in the ledger. To change it, change `backend` in the workspace configuration; the system does not switch it at runtime.

## ACP session backend

`acp` is an explicitly selected backend: it is not in the host-detection candidate set and is selected by env `ONLYNE_BACKEND=acp` or `backend = "acp"` in the workspace's `config.toml`. `session_command` is that agent's ACP launch command, for example `qoderclicn --acp`.

One agent process hosts every session for the role. The process is reused according to the rendered command, and sessions are distinguished by ids assigned by the agent.

The configuration surface is the `[acp]` table in the workspace's `config.toml`:

| Configuration file | Field | Default | Effect |
|---|---|---|---|
| `[acp]` in `<workspace>/.onlyne/config.toml` | `mode` | empty | Session mode passed to the agent through `session/set_mode`; an empty value uses the agent's own default |
| `[acp]` in `<workspace>/.onlyne/config.toml` | `model` | empty | Value of the model configuration setting; an empty value uses the agent's own default |
| `[acp]` in `<workspace>/.onlyne/config.toml` | `reasoning_effort` | empty | Value of the reasoning-level configuration setting; an empty value uses the agent's own default |
| `[acp]` in `<workspace>/.onlyne/config.toml` | `permission` | `deny` | The local response when the agent requests permission: `deny` rejects and records a fault, while `allow` permits |

A rejected permission request records a `permission` fault whose reason lists the rejected tool call and local policy; the same task's terminal state still enters the ledger normally.

### Completion reports (payload-v2)

The client completes ACP sessions. There is no `onlyne` CLI inside the agent session, and none is needed. `deliver` injects report instructions at the end of the prompt for every delivery. The first line of those instructions gives the absolute path `<workspace>/.onlyne/out/<task-id>.md` and prints the complete grammar verbatim. The instructions require the agent to write its result to that file before stopping: first write to a temporary name in the same directory, then rename it into place. Report body text follows the existing `out_head` rules: one line, collapsed whitespace, and a 200-character truncation.

Grammar v2 specifies that a report file contains one verdict line plus zero or more handoff lines (a single-line v1 file is also valid):

| Line | Meaning |
|---|---|
| `hop-done: <one-line result>` | verdict: the task is complete, and the body is the conclusion |
| `hop-failed: <one-sentence reason>` | verdict: the task failed, and the body is the reason |
| `hop-blocked: <one-line blocker>` | verdict: the task is blocked on an external dependency, and the body is what it is waiting for |
| `handoff: <target role> \| <one sentence for that role>` | One handoff line; zero to eight may appear. The text after `\|` is optional, and when omitted the verdict body is the delivered content |

A line beginning with `#` is a comment, and blank lines are ignored. Any other line that violates the grammar means the entire file is Invalid (fail closed: zero handoffs, zero routes). A single file may contain at most 16 lines and at most 8 handoffs; exceeding either limit is also Invalid. CRLF and bare CR are first normalized to LF, then lines are classified.

The client creates the report directory before delivery. If creation fails, that prompt has no instruction block, and the round settles normally as if the file were absent; the journal adds a `warning` record beside the `dispatch` record.

After the turn ends and every `session/update` for that round has been recorded, the client reads the report file once: it parses first, routes handoffs second, and deletes the file last. Completion values are:

| Report case | Result |
|---|---|
| File absent or unreadable | Preserve pre-contract behavior: outcome and head are derived from stopReason and the round's final assistant text |
| `hop-done: <nonempty>` | The report text becomes head; outcome is still determined by stopReason, and a round classified as an abnormal termination by stopReason keeps that classification; handoff lines are routed normally |
| `hop-failed: <nonempty>` | Outcome is failed; the report text is both head and fault reason; even a normal `end_turn` is downgraded; handoff lines are routed normally |
| `hop-blocked: <nonempty>` | Outcome and head come from the report's blocked body, but the task is not handed off: the work is unfinished and there is nothing to pass to the next role |
| Invalid (extra line, unknown prefix, over limit, empty file, bad UTF-8) | Outcome is cancelled; the fault reason begins with `acp payload invalid:` and states the category and line number; head is empty and there are zero handoffs. The file remains in place, so redelivering the same task after rewriting it can consume it |

The three verdicts `done|failed|blocked` reach the payload layer through the `head_kind` field of `Outcome::Finalized`, allowing the client to distinguish blocked from the other two.

The client routes handoffs over that role's existing server connection without impersonating a human request. A single routing failure (ACL rejection or target role absent from the local connection plane) records one `handoff_denied`: the journal records a `handoff_denied` event with `to_role` and the rejection reason, and the faults plane reports a fault with the same name. The verdict is not removed, the Outcome kind is unchanged, and the remaining handoffs continue.

One report can go to at most eight roles at once, and each role receives the body belonging to its own line: when the line contains `| <one line>`, the recipient reads that sentence; otherwise it reads the verdict body (`Handoff::text_or`, `crates/onlyne-proto/src/payload.rs:29`).

A hop records a handoff's depth in the chain. Handoff-chain depth remains open by product intent: neither the protocol nor the server imposes a hop-count limit, and hop travels with the envelope solely as causal history (`crates/onlyne-proto/src/envelope.rs:331-339`, `crates/onlyne-server/src/relay.rs:640`). The `allowed_targets` edges in `spec.toml` determine which roles can receive a handoff, and the server's ACL gate answers every send according to those edges (`crates/onlyne-server/src/relay.rs:379-393`).

There is one way for an operator to bound the chain: remove the corresponding `allowed_targets` edge and run `onlyne reload`.

Every read appends a `payload` record to that task's journal with the fields `task_id`, `path`, `payload_kind` (one of `done`, `failed`, `blocked`, `invalid`, or `absent`), `head`, and `handoffs` (the number of readable handoff lines in this round). A rejected report record also has an `error` field stating the rejection reason and line number. An absent report is recorded too, so the ledger shows whether that round reported anything.

Before the completion file is deleted, every handoff line worth routing gets a separate `handoff` record with the fields `task_id`, `to_role`, and `head`; a rejected handoff line becomes `handoff_denied` on the client side.

Completion facts enter the ledger through the single `dispatch::on_out` path: settle, `out_head`, acknowledgment, and the completion receipt are all emitted there. Every terminal task sends a receipt; when both the report and final text are absent, a `completion` row with empty body text is recorded.

### Local validation command family (`onlyne report`)

The same grammar parser (`onlyne_proto::payload`) is exposed as local CLI verbs that only read and write workspace files and open no socket:

| Verb | Behavior |
|---|---|
| `onlyne report path --task <id>` | Print the absolute completion-file path for the task, together with the three on-disk session paths `log:`, `events:`, and `content:` |
| `onlyne report check --task <id>` (or `--path <file>`) | Valid: print the verdict kind, head/reason, and every handoff line, then exit 0. Invalid: print `onlyne: <精确原因（含行号）>` plus the complete grammar to stderr and exit 2. File absent or unreadable: report `absent` or the read-failure reason to stderr and exit 2 (3 is reserved only for socket resolution) |
| `onlyne report write --task <id> --verdict <done\|failed\|blocked> --head <text> [--handoff <role\|text>]...` | Construct a valid report from its parts, write it atomically using a temporary name plus rename, and print the final path |
| `onlyne report validate --text <s>` (or `--from -` to read stdin) | Run the same parser on the string without touching a file |

`--workspace` uses the same location convention as socket resolution: start at the supplied directory and search its ancestors for `.onlyne/config.toml`; if none is found, use the supplied directory itself. The full grammar is embedded in `onlyne report --help` and `onlyne report check --help`, so an installed user can check the format without consulting documentation.

## Session content

ACP sessions have no terminal: the agent is a child process held by the client, and its session is invisible to processes outside the client. What remains is the on-disk journal. `<workspace>/.onlyne/logs/session-<task>.events.jsonl` has one JSON object per line, containing that agent's `session/update` notifications plus the client's own `dispatch`, `payload`, and `turn` records; `<workspace>/.onlyne/logs/session-<task>.log` is the human-readable rendering. `<workspace>/.onlyne/logs/content.index.jsonl` has one metadata line per record, recording its offset and length in the task journal, so role-level content sequence numbers continue after a client restart. All three are ordinary files, local permissions determine who may read or write them, and the client does not provide a live stream to any process outside the session.

## Windows shutdown

Windows has no SIGTERM / SIGHUP. `tokio::signal::windows::ctrl_c` connects to the existing SIGINT shutdown path. The supervisor performs shutdown by running `onlyne server stop` on the host containing the server root; spec hot reload uses `onlyne reload`. See the previous section for the exec-session child-process kill ladder.

On Windows, `.onlyne/run/s` is a marker file (content `v1:onlyne-<32hex>`), and the named-pipe name is derived from the lowercase sha256 of the path's lexical-absolute form. `--socket \\.\pipe\` passes through unchanged. `ERROR_PIPE_BUSY` is retried within the CLI `--timeout`. On Unix, AF_UNIX remains a filesystem UDS.

**中文**

Onlyne 运维以 server 账本、client 工作区、admin 本地 socket（规范名 `.onlyne/run/s`；路径不超过 103 字节时绑在这一条，超限时绑到系统临时目录下的短派生路径，实际服务的路径记在同目录的 `run/socket` 标记里）为边界。

## 值守入口

`onlyne status` 通过 admin 本地 socket 读取 server 状态。

`onlyne roles` 读取 role 注册表和在线状态。

`onlyne sessions` 读取 session 投影，答案就是心跳落下的那一行镜像，`updated_at` 是它的年龄。

`onlyne sessions --fresh --task <task>` 现场取一次真值：server 把既有的 `control` op `probe` 发给该 task 的属主 client，等这一行越过读取开始时的 `(generation, seq)`，再按这一行作答。admin 词表不加动词，client 侧也不加代码。

等待的上界是这次读取自己的 `--timeout` 减去帧往返预留的 250ms；probe 没落地时照常按存下的镜像作答，读取不会超出这个界，也不会为了等它而放宽这个界。

`--fresh` 的答案在行上带 `fresh` 字段：`probed` 是 probe 落地后的观察，`offline` 是没有可问的对象（没给 `--task`、该 task 没有行、没有 client 拥有它、或属主不在线），`unanswered` 是 probe 出去了但界内没有重发布。三种情况都答那一行，不报错、不挂起。

`--fresh` 必须带 `--task`：fresh 读取问的是一个具名 task 的 client。

不带 `--fresh` 的读取与从前逐字节相同：不发 control 帧、不等待、答案里没有 `fresh` 键。

`onlyne ledger` 读取投递账本。

`onlyne faults` 读取 faults 表。

`onlyne watch` 读取 durable 与 advisory 事件流。

`onlyne history` 回放事件记录。

`onlyne spec_diff` 对比运行中 spec 与磁盘 spec。

`onlyne tui --server-root <root> --once --page <1|2> --state <active|all>` 渲染一帧纯文本后退出。`active` 是默认过滤，只保留活动视图；`all` 还包含已结清的 session 与账本行。第 2 页配合 `--state all` 可以直接读到已结清行的 `reason=<text>`。

## 服务路径的读法

每个守护进程绑定 socket 时把实际服务的路径发布在 `<owner>/.onlyne/run/socket`（mode `0600`，一条绝对路径加一个换行）。操作者读这条路径有三个入口：

`onlyne-client status --workspace <dir>` 打印 `socket <path>`，字段值就是这条服务路径。

client 日志在启动时点名这条路径：短路径场景一行同时给出规范路径、其字节长度、服务路径与 marker（`adapter socket moved to the short path`）；规范路径场景一行给出 socket 路径（`adapter socket serving`）。server 侧同口径：短路径场景一行给出 served 与 canonical 两条路径及其长度（`the run socket is served from a short path; the marker names it`），常规场景一行 `the run socket is open`。

`cat <workspace>/.onlyne/run/socket` 直接读 marker 文件。

session 进程带着 `ONLYNE_SOCKET` 启动，值是这条服务路径；role pane 里的 `onlyne` 命令凭它直达 socket。CLI 的解析次序是 `--socket` > `ONLYNE_SOCKET` > `--server-root` > `--workspace`/cwd 上行查找，查找以 `.onlyne/run/s` 或 `.onlyne/run/socket` 认定属主目录，路径经 `socket_path()` 解析。

bind 失败的 client 以 exit 1 结束，stderr 一行 `onlyne-client: bind the workspace socket <规范路径>: <明细>`，明细给出服务路径、两条路径各自的字节长度与 OS 原因；一个绑不上 socket 的 client 选择退出，保持 TLS 链路静默重试的循环已移除。绑定成功之后的 `accept` 错误以 `error` 级记日志（`adapter socket accept failed; retrying`），每 100 毫秒重试一次，listener 保持在手。

验证链路由 `crates/onlyne-testkit/e2e/socket-path-length.sh` 钉住：垫深的工作区、短服务路径、marker 发布、规范路径保持空位、任务端到端结清。

## 配置加载

spec.toml 或角色工作区 config.toml 里的未识别键被忽略，进程照常启动；每个被忽略的键在加载时于 daemon 日志落一行 `tracing` warning。真实键的取值错误仍是拒绝启动的硬错误。推论：拼错的键静默回退到默认值，除那行 warning 外无其他信号。

一次版本升级后，role client 与其 server 必须跑同一 build：`hello` 回复少了一个字段，二者不匹配时连不上。升级时把 server 和每个 `onlyne-client` 一起重启。

## 并发度

同 role 多 session 并行的旋钮是 `[[client]].max_sessions`。

`max_sessions` 的语义是同时在飞 session 上限。

每个 task 拥有独立 session。

role 达到 `max_sessions` 后停止 pull 新任务。

满容量的 role 把 `pull` 换成 `control_only = true` 继续发。

control 行与任务行共用一条 pull 队列，容量闸门挡的是任务。

`recycle`、`cancel`、`focus` 是腾容量和看现场用的命令，恰好要在满容量时到达，所以 server 在这一路只交 control 行，任务行的 `queued` 状态与 ticket 都不动。

server 保留挂账并在下次 pull 时再次 offer。

`onlyne-client init` 的种子值是 `max_sessions = 1`。

种子值保护单 pane 手工环境。

并行度按 role 在 spec 里显式设置。

spec 改完后执行 `onlyne reload` 生效。

存量 client 收到 `SpecReloaded` 事件后自动刷新 role slice。

存量 client 刷新 role slice 后立即使用新的 `max_sessions` 闸门。

存量 client 无需重启。

`crates/onlyne-testkit/e2e/reconnect-requeue.sh` 用 `max_sessions = 2` 和三条 task 覆盖挂账再 offer 路径。

满容量时 control 仍到达这一条，由 `crates/onlyne-server/tests/delivery.rs` 的 `a_control_only_pull_hands_the_command_and_leaves_the_work_queued` 在协议面钉住，并由 `crates/onlyne-testkit/e2e/herdr-live.sh` 的 d 步在活宿主上验一次：该 case 的 role 用种子值 `max_sessions = 1`，唯一槽被一条 `sleep` 占满，`control focus` 依然落到 session 的 pane。

## 焦点

`onlyne control --from <role> focus --task <id> --force --yes-i-am-supervisor-not-other-role` 把某个 session 的 pane 摆到前台。TUI 的入口是 `F`，作用在选中的那一行上。

控制平面用 `ControlOp::Focus{task_id}`，账本行 `kind = control`。命令落到 session 的 `backend_ref`，herdr 后端按三段链路走：`herdr workspace focus <W>`、`herdr tab focus <T>`、第三段按 pane 的来历分岔 —— managed agent 走 `herdr agent focus <pane_id>`，`herdr pane run` 拉起的 shell pane 走 `herdr pane focus --pane <base_pane> --direction <split_direction>`，这两个值是分屏时记下的。`base_pane` 与 `split_direction` 存在 `backend_ref` 里，所以锚点跟着 pane 活。

`herdr pane get <pane_id>` 是确认那一步。`result.pane.focused` 为 true 才算送达；落在别处时命令报错，并指名当前持焦的 pane。`focus()` 失败记一条 `Report::Fault{kind:"focus"}`，TUI 把后端原文打在这一行的反馈位。

`--from` 是 admin 面的全局旗标，写在 `control` 之后。焦点命令的 ACL 与投递同口径：对目标有 `send` 边的 role 掌握该目标会话的控制权，`ControlOp::Broadcast` 需要全局边。

herdr 后端按 label 认 workspace（`onlyne:<cluster>`），按名字认 tab（role 自己的名字）。想让 session 落进手上这个 workspace 与 role tab，操作者在拉起 session 之前先改名：`herdr workspace rename <WORKSPACE_ID> onlyne:<cluster>`、`herdr tab rename <TAB_ID> <role>`。label 对不上的 workspace 会拿到第二个 workspace，tab 名对不上会拿到第二个 tab，这时 client 打一条 warning，点名该 label 与新建出来的 workspace。

## 故障恢复

fault 是 server 记录的可审计事实。

fault 进入 `faults` 表。

fault 通过 advisory `Event::Fault` 推给观察者。

`onlyne repair inspect --task <id>` 读取一条任务的恢复上下文。

`onlyne repair adopt --task <id> --backend <backend> [--backend-ref <值>] --reason <reason>` 换掉这一行 desired 里的 backend 绑定，行内的 session id 与 generation 保持原样，seq 前进一格。要把任务搬到另一个 session，用下面的 `rebind`。

`onlyne repair rebind --task <id> --session-id <session> --backend <backend> [--backend-ref <值>] --reason <reason>` 重写任务的 backend 绑定，把行内的 session id 换成给定值，generation 加一、seq 归零，旧 generation 的上报从此不再被采信。

两个动词的 `--backend-ref` 同一规则：能整体解析为 JSON 的取值按解析结果上线（pane 引用这类对象形值因此可直接写 `--backend-ref '{"id":"p-7"}'`），其余文本按一个 JSON 字符串上线，旗标缺省上线 null。服务端与 client 按对象消费该值（`crates/onlyne-server/src/faults.rs:253-257,273-277`、`crates/onlyne-client/src/session/dispatch.rs`）。

`onlyne repair retry --task <id> --reason <reason>` 把可重试任务送回队列。

`onlyne repair fail --task <id> --reason <reason>` 把任务收敛为失败。

`onlyne repair close --task <id> --reason <reason>` 关闭恢复工作。

`onlyne repair ack --fault-id <fault-id> --reason <reason>` 确认一条 fault。

repair 族走 `<server-root>/.onlyne/run/s` 的 admin 面；树深过 103 字节界限时走 `run/socket` 记下的那条短服务路径。

repair 族不经过 role 工作区的 adapter socket。

## 投递与重投

Onlyne 的 SQLite 数据库要放在本地文件系统。不要把 server root 或 role workspace 放在 OneDrive、Dropbox、iCloud、网盘或其他同步目录。SQLite 依赖 WAL 文件与本地文件锁；Onlyne 启动时不运行 `quick_check` 或 `integrity_check`，损坏的 `state.db` / `client.db` 会让 daemon 在首个数据库操作失败。先停止所有 client/server，再把整个 root 原样复制到本地目录，在副本上检查数据库；原库不要执行 checkpoint、VACUUM 或 repair。

role link 死亡时，服务端把该 role 的 `in_flight` 投递行重投回 `queued`，等待下一次 pull 再交付。

新 link 落地时的接管重投走同一条路。`hello` 的 `live_tasks` 字段申报该 client 内存里仍活着的会话任务；被申报的行保持 `in_flight`，其 delivery ticket 改挂新 link 的 generation，此后该 link 终止时照常被重投。

被申报的会话若在结清之前死亡，client 发布 `exited` 投影，服务端见到与该会话 ticket 同 `session_id` 的 `in_flight` 行时把该行重投回队列，同样经过下面的闸。

自动重投受两个预算旋钮约束；手动 `onlyne repair retry` 不经过闸。回到 `queued` 的
`in_flight` 行按 `requeue_ttl_secs` 再按 `requeue_max_attempts` 判定。收件 role 没有
live connection、且从未被 pull 的 task、completion、control 行，在 `requeue_ttl_secs`
非零时按同一 TTL 过期；默认值为 0 时保持排队。

| 配置文件 | 字段 | 默认 | 作用 |
|---|---|---|---|
| `<server-root>/.onlyne/spec.toml` 的 `[server]` | `requeue_max_attempts` | 0 | 一条行允许的自动重投次数上限，0 为不限；超限的行落 `rejected`，reason 为 `requeue_exhausted` |
| `<server-root>/.onlyne/spec.toml` 的 `[server]` | `requeue_ttl_secs` | 0 | 回到队列的行与收件 role 无 live connection 的未拉取行允许的 TTL，按入队时间计，0 为关闭；超龄的行落 `expired`，reason 为 `requeue_ttl` |

先判 TTL，再判次数，两者都各发一条 `ledger_state` 事件。

`reason` 的读法：`onlyne ledger` 的行键为 `msg_id`、`task`、`state`、`reason`、`out_head`、`body`、`family`、`hop_budget`；该键只在这一行有值时出现，无值的行与列加入之前逐字节一致。TUI 第二页的 task 详情面板在账本行尾追加 `reason=<text>`。落进这一列的取值：`requeue_exhausted` 与 `requeue_ttl` 来自上面两道闸，`expired` 来自到期扫描，`session_dead` 来自 client 结清掉线 session 时写下的拒收（见「会话残影与属主判定」一节），`operator cancel` 与 `operator recycle` 来自 client 在操作者的词无人作答时自行结账、并拒收仍握在手里的投递行（见下方 control 一节），pane 后端（`herdr` / `orca` / `zellij`）在开页前拒收协议 `session_command` 时整句拒收文案落 `rejected` 行（见「Headless（exec）会话」一节末段）；操作者经 `onlyne reject --reason` 或 `onlyne repair fail --reason` 自填的文本原样进这一列，`onlyne ack` 收下时该文本随结清事件走，行上的 `reason` 保持原样。字符串 `operator ack` 是 faults 表 `reason` 列的用例数据（`crates/onlyne-store/src/tests.rs` 的 `update_fault_state` 用例），账本列没有它的记录。

push 投递与 pull 投递的 `in_flight` 翻面都各有一条 `ledger_state` 事件；离线读账的 ledger 状态与会话投影在任何采样点互相对得上。

完整链路（server 在活 link 下死亡、client 重连接管、单一会话自然结清）由 `crates/onlyne-testkit/e2e/requeue-claim.sh` 在真实进程上验证。

本角色已经以 `Done` 结项的 task 再次投来时，client 认得它。判定读 `client.db` 的会话终态，入口是 `onlyne-client` 的 `DispatchState::task_completed_here`，动作在 `accept_delivery` 的容量闸之前：这一行就地 ack，`accepted = true`，reason 为 `task already completed by this role`，会话不 stage，容量不占。该 reason 进 `ledger_state` 事件，行本身落 `acked`。日志面同一时刻记一条 `redelivery of a finished task settled without running it`，带 `msg_id` 与 `task`。

这道判定读的是 `Done`。会话被终止、崩溃或本地记为 `Failed` 的 task，只有仍有符合条件的 `queued` / `in_flight` 投递行时才可由 `onlyne repair retry` 重投；已被 `session_dead` 等终态拒收的 task 要重跑，操作者用带门禁的 `send` 动词发新 task。已 `Done` 的行等到的是这条 ack 和一次空跑。

## 拒收面

`onlyne ack --msg-id <id> --reason <text> --force --yes-i-am-supervisor-not-other-role` 把一条投递结为 `acked`。

`onlyne reject --msg-id <id> --reason <text> --force --yes-i-am-supervisor-not-other-role` 把一条投递结为 `rejected`。

`--reason` 在两个动词上都是必填。

`--op-id` 在两个动词上都可选。

拒收的 reason 落账本行。

两个动词都走角色工作区的 adapter socket。

`--request` 被这两个动词拒绝。

插件在 `assign` 上回 `accepted = false` 时，client 以同一 `msg_id` 入队一条拒收 ack。

该拒收 ack 与 completion 共用 durable intent 队列，断连后在重连时补发。

拒收理由取插件给的 reason，插件没给时记 `assign rejected`。

已结清的投递再收一次 ack 或 reject，服务端回同一状态事件。

## 会话残影与属主判定

lifecycle 属主是 role 自己的 client 进程。

client 死亡期间无人代该 role 判定 session 生命周期。

属主进程活着时，掉线的 session 由 client 结清：plugin 连接断开、`reconnect_grace_secs` 超期后，退役它留下的 session。

结清写两条：该 task 落 `failed`，它占着的投递行以 `session_dead` 拒收；退役的同一趟再把该 session 自己的投影随一条 heartbeat 报告发出，server 行随即读 `exited`，不必等观察器的 `stale_working` 或 `heartbeat_missing`。

该拒收是终态；要再次执行这项工作，创建新 task。`repair retry` 只处理符合条件的 `queued` / `in_flight` 行，task 行已结算时返回 `conflict`。

账本判定以 `kind=task` 行为准；`kind=completion` 行记录回执传输与 `out_head`，不会重开已 `rejected` 的 task。投影的 `(generation, seq)` 是各写入方自己的水位，client 与 server 不要求相同；终态读取比较 task outcome、lifecycle、resource 和 generation。

重启的 client 不对旧账做终态判定：`Acked` 的行是它自己已经答过的，重启不上报它们的死，也不替别的属主判定。

残影判定与冻结上报有五个旋钮：

| 配置文件 | 字段 | 默认 | 作用 |
|---|---|---|---|
| `<workspace>/.onlyne/config.toml` | `stall_report_secs` | 1800 | 会话投影 tuple 冻结时长上限，client 据此上报 `stalled` fault，0 关闭，单位秒 |
| `<workspace>/.onlyne/config.toml` | `reconnect_grace_secs` | 60 | plugin 连接断开后允许其离席的时长，超期由 client 退役它留下的 session 并结清它欠的那件事；仍绑着 task 时该 task 落 `failed`、其投递行以 `session_dead` 拒收，0 关闭，单位秒 |
| `<server-root>/.onlyne/spec.toml` 的 `[server]` | `stale_watch_secs` | 60 | server 观察器扫描周期，单位秒；0 关闭观察器 |
| `<server-root>/.onlyne/spec.toml` 的 `[server]` | `heartbeat_grace_secs` | 90 | 属主在线时 `working` 行允许的心跳静默时长，单位秒 |
| `<server-root>/.onlyne/spec.toml` 的 `[server]` | `ghost_sweep_secs` | 60 | server ghost sweep 扫描周期，单位秒；0 关闭这一趟 |

server 侧观察器按 `[server].stale_watch_secs` 周期扫描，一次扫描跑两个探测器。

探测器一扫描 `working` 且属主离线的行。

离线探测器超过 600 秒记录 kind 为 `stale_working` 的 fault。

探测器二扫描 `working` 且属主在线的行，判据是心跳新鲜度。

pi 插件每 10 秒发一个 heartbeat 包，client 每收到一拍就在自己的 heartbeat 报告里重发一次完整投影，服务端行的 `updated_at` 随心跳前进。

不等下一拍心跳、要当下观察时用 `onlyne sessions --fresh --task <task>`：它把 `probe` 发给属主 client，等这一行越过读取开始时的水位，插件用一条 heartbeat 应答这个 `probe`。

静默一拍即翻面的 no-op 心跳同样携带存活事实，client 对其抬升本地版本号并重发，健康会话的 `updated_at` 保持新鲜。

属主在线、行龄超过 `[server].heartbeat_grace_secs`（默认 90 秒）、且本进程见过该行的写入时，探测器二记录 kind 为 `heartbeat_missing` 的 fault。

`heartbeat_missing` 的行同时出现在 `sessions` 答案的 `heartbeat_stale` 字段上，TUI 呈现为 `working+stale`。

同一任务已有未确认的同类 fault 时，探测器二不再重复记录。

server 启动时把当时 `working` 的行全部登记为已见，上一进程遗留的静默行同样可被标记。

completion 落定为 `exited` 之后同 generation 的 heartbeat 把行抬回 `working` 时，观察器记录 kind 为 `heartbeat_after_complete` 的 fault，投影照常应用。

两个探测器都只记 fault 并推送 advisory `Event::Fault`。

观察器之外另有一趟 ghost sweep，按 `[server].ghost_sweep_secs` 周期扫描，首个 tick 落在半个周期处。

它读取镜像里 `working` 的行，并把每一行与该任务自己的 ledger 行对齐。

ledger 行已落终态的那一行 `working` 镜像是化石：账已结清，镜像还留着旧字节。

一趟扫描只在镜像没有 outcome 时采用 task ledger 的判定：`acked` 读作 `done`，`rejected`
与 `expired` 读作 `failed`。镜像已经发布的 outcome 保持原值。

写入走 `repair` 族共用的那条结清路径：重写 `observed_json`、抬升 `seq`、落库、推送 durable `session_state`。

ledger 行仍是 `queued` 或 `in_flight` 时，这一趟留下该行原样。

每移动一行就在 `ghost_sweeps` 审计表落一行。该行每个字段都取自服务端已经持有的行：task、role、session、generation、`seq_before`、`seq_after`、outcome、evidence、`swept_at`。

`evidence` 文本是标签加上为这次写入作证的那个 ledger 状态，例如 `task_settled:acked`。

`onlyne ghosts [--limit N]` 读这张表，最新的一趟在前。它是 admin 面的读取，client 面上拒收。

`stalled` 由 client 上报，走 role 的 `report` 面：会话的投影 tuple 连续超过 `stall_report_secs` 没有任何一次 `Applied` 变化时，client 发一条 kind 为 `stalled` 的 `Report::Fault`，带 task 与 session 身份。

no-op 心跳抬存活水位，不抬进展水位；`stalled` 只看后者。

`stalled` 的 reason 文本是 `no applied progress`。

一条已结清的任务走不进这条判据：进展时钟只由 task 分配建立，`note_applied` 只刷新已经建立的时钟，plugin 在完成回执之后送出的最后一拍 `agent: "idle"` 心跳落在空处。到期扫描与发送边界各派生一次 lifecycle——存储元组与 `task` 表里该任务的结清值一起过 `project`——得出 `exited` 的 task 被抑制并被遗忘；连接释放顺手忘掉它服务过的每个 session 的时钟。

同一冻结 episode 只报一次，下一次 `Applied` 解除去重。`stall_report_secs = 0` 关闭这条判定。

行不翻面：`stalled` 只落 faults 表并推事件，恢复决策留给 supervisor 与 repair 族；会移动镜像的只有 ghost sweep 一趟，口径见本节末。

连接断开宽限走另一个旋钮。plugin 连接在没有 `detach` 帧的情况下结束时，client 保留它的 session 与宿主资源，`reconnect_grace_secs` 从这一刻起计。

窗内重连清掉这个时钟：agent 回到它原来那个 session，照常领下一个任务。

超过窗口仍未回来时，client 退役它：关掉宿主资源，吐出容量槽位。

退役前先结清这个 session 欠的那件事：仍绑着 task 时，该 task 落 `failed`，它占着的投递行以 `session_dead` 拒收；退役的同一趟再发出该 session 自己的投影，server 行随即读 `exited`。未绑 task 的 slot 只退役。

退役的 reason 取该 session 名下 task 已落的终态，无终态可取时记 `Fault`。`reconnect_grace_secs = 0` 关闭断连宽限这条判定。

断连宽限与 attached-but-silent 是两条退役路径。连接断开使用 `reconnect_grace_secs`；连接仍在但绑定的未结清 task 连续三个 heartbeat interval 没有 accepted frame 时也退役。两条路径都结清 task 为 `failed`、以 `session_dead` 拒收持有投递、关闭资源并发布退出。

一个更新的 session 已经在服务同一 task 时，旧 id 的连接回来即降级为只读：它不再收到 `assign`、`deliver`、`render_send`，也不占该 session 的投递面。

只读连接本身仍被接纳，它送出的 `send` 帧不入 durable 队列、不发往服务端，而是攒进该 task 的缓冲，等一次合并。

降级只在那条更新的连接在线期间成立：它结束时，为该 session 攒着的第一个连接即被提升为其 transport，只读标记同时解除，所以插件抢在 client 察觉旧 socket 已死之前重挂不会被永久静音。


重试那条 session 结项时，缓冲与它自己的 handoff 按下游 role 合并成一条：一个 role 一条 envelope，正文每行带来源标注，`[retry]` 是结项那条 session 写的，`[zombie]` 是攒着的旧连接写的。

合并投递之后，只读连接收到 `bye` 并被摘掉；它若还占着自己的 slot，该 slot 以 `Replaced` 退役。结项的账只付一次，这一步不再 settle，也不再 release。

一条例外：促成这次合并的那份 report 就是从这条只读连接上收进来的，那么它在这一轮收不到 `bye`。client 先把这条 report 的应答写出去，连接的收尾交给它自己的 `detach` 帧或 socket 结束。插件对 `bye` 的处理是断开 socket 并把所有在途请求判为失败，抢在应答之前的 `bye` 会把一笔已经落账的完成读成失败，agent 因此重发终态。顺序由 `crates/onlyne-client/tests/scenarios/reconnect.rs` 的 `a_read_only_completion_is_answered_before_any_bye` 钉住。

faults 表的 kind 字段保存 `stale_working`、`heartbeat_missing`、`heartbeat_after_complete`、`stalled` 文本。

server 侧观察器不改 ledger 状态。

server 侧观察器不触发 retry。

server 侧观察器不触发 fail。

server 侧观察器遵守 §8 的零政策红线。

ghost sweep 在这条红线内移动一类行：镜像仍读 `working`、而该任务自己的 ledger 行已落终态的那一类。

它写进镜像的判定来自那条 ledger 行，同一趟把该任务名下仍未结清的投递行一并结清——`kind = completion` 的回执除外：回执是这次结算自己的记录，结局就写在它的 `out_head` 上，拒它等于把结局从账面上抹掉；收件人的 client 回来就会 ack 它，没有 client 的 role 按「回执照排队」的口径留着（见「拒收面」）。

哪一类行会落进这趟的范围，取决于后端。服务端把**不带投影的裸心跳**合成成 `working` / `running` / `attached`（`crates/onlyne-server/src/projection.rs`），所以只有在结算之后仍有裸心跳上行的后端，才可能把镜像从 `exited` 滑回 `working`。实测：pi + orca 在任务结算后不再送裸心跳——完成路径本身会发布一次（lifecycle 由 `Done` + `Accepted` 推成 `exited`），随后退休再发布一次（`agent` 走 `gone`、资源走 `closed`），两次之间没有第三帧；而 acp 这类长命后端的 agent 进程可以活过会话，形状在那里才成立。

另一类 `working` 行它不动：属主离线、而任务仍未结清的那一类。

给这类行写判定就是替活着的活儿定终局，它该得的重排队列会被吞掉。

`stale_working` 仍是这类行唯一的输出，恢复决定仍归 supervisor 与 `repair_*` 族。

存活判定的信源是 pi-onlyne 心跳包本身，pane 与宿主终端的存活状态不在服务端判定面内。

恢复决定归人和 supervisor 角色。

人使用 repair 族执行 `inspect`、`adopt`、`rebind`、`retry`、`fail`、`close`、`ack`。

supervisor 角色使用 control 动词执行恢复动作。

`onlyne control --task <id> recycle --reason <text> --force --yes-i-am-supervisor-not-other-role` 与 `onlyne control --task <id> cancel --reason <text> --force --yes-i-am-supervisor-not-other-role` 的 reason 是必填。

admin 面的 `onlyne control` 不要求 `--to`：缺省时 CLI 先读该任务的 session 行，把控制送到属主 role 那里；显式给出 `--to <role>` 时直接用它，不再多读一帧。

任务没有任何 session 属于某个 role 时，命令在写任何东西之前拒收，退出码 4，stderr 逐字 `onlyne: no session owns task <id>; pass --to <role> to say where the control goes`。

`recycle` 与 `cancel` 以操作者的词结掉任务：client 请插件收尾并关掉宿主资源，插件的报告落地即结账。插件始终不回答时，client 在三个心跳间隔（`CONTROL_SETTLE_BOUND`）之后按操作者的词自行结账，`cancel` 落 `cancelled`、`recycle` 落 `failed`，并把仍握在手里的投递行以 `operator cancel` / `operator recycle` 拒收。

超时结账只写第一个判定：任务已被别的门结定时它什么都不写、也不发布；store 拒写时该词按原时刻重新记账，下一拍重试。若另一个门已经结清 task 而镜像仍是非终态，ghost sweep 后续按 task 的终态账本移动这条旧镜像。

supervisor 角色的 control 动词需要 spec 授权。

spec 未声明 `admin = true` 的角色时，admin 身份不存在。

admin 身份的 control 免 role 边表判定。

admin socket 上的 `onlyne control` 以 admin 身份执行。

非 admin 角色的 `control` 仍要属主身份或 `admin = true` 的边。

角色间没有 control 授权时，代 hop 的 `control cancel` 返回 `forbidden`。

`control cancel` 返回 `forbidden` 是设计行为。

此时恢复入口是 admin 本地 socket 的 repair 族。

```bash
onlyne --server-root <server-root> repair inspect --task <id>
onlyne --server-root <server-root> repair fail --task <id> --reason session_dead
```

第一条命令读取残账和 session 投影。

第二条命令把任务收敛为失败并保留 `session_dead` 原因。

## supervisor 维护指令集

`send`、`reply`、`handoff`、`complete`、`ack`、`reject`、`control` 这七个动词要求 `--force` 与 `--yes-i-am-supervisor-not-other-role` 同时在场。

两个旗标缺任何一个，命令在解析 socket 之前退出 2。

拒收文案点名角色在会话里该走的插件工具：`send` 是 `onlyne_send`，`handoff` 是 `onlyne_handoff`，`complete` 是 `onlyne_complete`，其余四个动词由插件本身代答。

CLI 门属于 supervisor，也属于会话不挂插件的 `exec` 角色。

`exec` 角色用 CLI 形态，并以这两个旗标声明自己。

`repair *`、`ledger`、`sessions`、`roles`、`faults`、`watch`、`history`、`status` 这些读动词、`reload` 与 `shutdown` 不带这两个旗标。

## 任务家族与元信息

一个任务家族带着自己的元信息，在起跑处通过带门禁的 `send` 动词写入：

```bash
onlyne send --hop-budget <n> --label <k=v> --deadline <rfc3339> \
  --force --yes-i-am-supervisor-not-other-role
```

`--hop-budget <n>` 记下这一族能花的跳数，`--label <k=v>`（可重复到 8 条）记下脚本要读的自由键值，`--deadline <rfc3339>` 记下整族的墙钟期限。

`family` 是家族根任务的 id，一跳一传、永不改变。子任务继承 `hop_budget`、`origin`（发起根任务的角色）、`deadline` 与全部 `labels`，`hop` 取父行加一。

继承只发生在 `Causality::child_of` 一处：CLI 的 `handoff` 与插件的 `onlyne_handoff` 都走它，两条入口造出的链形状因此一致。

`onlyne ledger` 多印 `family` 与 `hop_budget` 两个键；没有值的行不出现该键，旧行与列加入之前逐字节一致。

`labels` 是核心唯一不解释的字段：上限 8 条，键不超过 32 字节，值不超过 256 字节，越界由 `Envelope::validate` 拒收并点名字段。

ledger 表新增的列走 in-place 加列，与 `expires_at`、`requeued` 同样处理，因此 server 的 schema marker 仍是 4，已有的 state.db 不必重建。

## 宿主资源回收

一条会话结束，它的宿主资源跟着回收：herdr pane、Orca 标签页、zellij session、exec 子进程在“该会话不持任务且无 plugin transport 挂载”时由 client 关闭。会话结清后留在 role tab 里的空 shell 由这三条路径收走，手工 `herdr pane close` 退到兜底位置。

三条触发路径：

- plugin 优雅 `detach`：这条连接服务过的每个空闲会话就地关闭资源。
- 结清且 agent 未挂载：关闭发生在结清那一刻。
- 250 ms readiness tick：扫描被跟踪的会话。一个会话是否结束是派生出来的：存储元组与 `task` 表里该任务的结清值一起过 `project`，得出 `exited` 才关掉，reason 由 task 表的那个结清值推导（`done` 得 `Completed`，`failed` 得 `Fault`，`cancelled` 得 `Cancelled`；`pending` 或无记录不产生 reason）。会话行本身不再存有 lifecycle，也不再自报任务结果。

会话结清后不再接新任务：一单一个 session，槽位随即归还，不再占用 `max_sessions`，宿主资源按上面三条路径收走。一条保留路径：连接在 plugin 未发 `detach` 的情况下断开，该 agent 还可能重连。

每一次回收在存储资源状态仍为开时先经 `backend.attach` 刷新过期 ref，投影 `resource_closed`，并在 client 日志记一行 `retiring idle session resource`，字段是 `task`、`backend`、`resource`、`reason`；槽位随后从跟踪表里移除。关闭失败落一条 warning，run 照常继续。

herdr 的关闭是幂等的：`herdr pane close` 回 `pane_not_found` 记为成功，日志落一行 debug `herdr pane already closed`，字段 `task` 与 `pane`。一个已经消失的 workspace 在此之后读作已关闭。

`stalled` 的含义因此收窄到真实静默：一条已完成的任务带 `no applied progress` 的 `stalled` 从这条面上消失，fault 表里的 `stalled` 只描述仍在跑的会话。判据细节见上一节的进展时钟条目。

### Orca 会话

每个 task 新建一个 Orca terminal。`attach` 只刷新已保存的 terminal handle，不把消息转发到任意已有 session。启动前检查 `orca status --json` 指向预期运行中的 Orca app，核对 client 从预期 Orca tab 继承的 `ORCA_WORKTREE_ID`，并确认 `orca` 解析到预期 CLI 且能连到该 app。CLI 返回 `[single-instance]` 时，这是启动当下拒绝创建 terminal。`session_dead` 在 client 后续 retirement sweep、slot 已存在时才会出现。投递已经因 `session_dead` 进入终态时，修复宿主后要运行这项工作必须创建新 task。

## Headless（exec）会话

`exec` 是无头后端的正名；`headless` 只是 parse 别名，投影与事件里的 backend 字符串仍是 `exec`。选择链是 env `ONLYNE_BACKEND`（非空）> 工作区 `config.toml` 的 `backend` > auto。`exec` / `acp` / `fake` 不会被宿主探测选中，须点名启用；`headless` 随 `exec` 同理。

会话子进程的 stdout/stderr 并进 `<workspace>/.onlyne/logs/session-<task>.log`。进程退出时，持柄 `probe` 把该文件尾部最多 200 行（先截约 16KiB 再按整行切）写入 `ResourceProbe.detail.output_tail`；log 缺失或读失败则省略该键，`exit` 码仍在。

关闭阶梯：

- unix：会话是独立进程组。`close` 用 `kill(2)` 只打记录的 pgid（`backend_ref.pgid`，与 leader pid 相同），先 `SIGTERM`，宽限 5 秒后再 `SIGKILL`，最后 `child.kill` 收尸。组信号失败时退回到对 leader pid 的同名信号。pid 0/`-1` 拒绝发送（那是“本进程组 / 一切可杀进程”，不是会话）。从不按 cmdline 通配杀进程。
- windows：spawn 带 `CREATE_NEW_PROCESS_GROUP`，停机先 `GenerateConsoleCtrlEvent(CTRL_BREAK)`，宽限后再 `child.kill()`（TerminateProcess）。客户端没有控制台时 CTRL_BREAK 失败，直接走终止。Windows 没有 SIGTERM；关停由 supervisor 在 server root 所在主机执行 `onlyne server stop`。

`pi --mode rpc` 是这条后端的典型 `session_command`：stdin 由 client 持开（EOF 对 rpc 意味着操作者离开），stdout 进 session log，消息面走 adapter socket，不走子进程的 stdio。

```toml
# <workspace>/.onlyne/config.toml
backend = "headless"

# <server-root>/.onlyne/spec.toml [[client]]
session_command = ["pi", "--mode", "rpc", "--session-id", "{session}"]
```

协议类 `session_command`（`pi --mode rpc`、`agent --acp` 这类在自己的 stdio 上说 JSON-RPC 的命令）只认 `backend = "exec"` 与 `backend = "acp"` 两种配置；写成 `herdr` / `orca` / `zellij` 时 client 在投递处拒绝，拒绝文案作为 reason 落进 ledger。改法是改工作区配置的 `backend`，运行期不会替你换。

## ACP 会话后端

`acp` 是显式选择的后端：它不进入宿主探测的候选集，由 env `ONLYNE_BACKEND=acp` 或工作区 `config.toml` 的 `backend = "acp"` 指定。`session_command` 是该 agent 的 ACP 启动命令，例如 `qoderclicn --acp`。

一个 agent 进程托管该 role 的全部会话，进程按渲染后的命令复用，会话按 agent 分配的 id 区分。

配置面是工作区 `config.toml` 的 `[acp]` 表：

| 配置文件 | 字段 | 默认 | 作用 |
|---|---|---|---|
| `<workspace>/.onlyne/config.toml` 的 `[acp]` | `mode` | 空 | 会话模式，经 `session/set_mode` 交给 agent；空值用 agent 自己的默认 |
| `<workspace>/.onlyne/config.toml` 的 `[acp]` | `model` | 空 | 模型配置项的值；空值用 agent 自己的默认 |
| `<workspace>/.onlyne/config.toml` 的 `[acp]` | `reasoning_effort` | 空 | 推理档位配置项的值；空值用 agent 自己的默认 |
| `<workspace>/.onlyne/config.toml` 的 `[acp]` | `permission` | `deny` | agent 请求权限时本机的答复：`deny` 拒绝并落一条 fault，`allow` 放行 |

被拒的权限请求落一条 `permission` fault，其 reason 列出被拒的工具调用与本机策略；同一任务的终态照常进 ledger。

### 结项报告（payload-v2）

ACP 会话的结项由 client 完成，agent 会话内没有 `onlyne` CLI，也不需要它。`deliver` 在每次投递的 prompt 尾部注入一段报告指令。这段指令首行给出绝对路径 `<workspace>/.onlyne/out/<task-id>.md`，并原样打印整份文法。指令要求 agent 在停止前把结果写进该文件：先写同目录的临时名，再 rename 进位。报告正文的取值沿用 `out_head` 的既有规则：单行、空白折叠、200 字符截断。

文法 v2 规定：一个报告文件由一行 verdict 加零或多行 handoff 组成（v1 的单行文件同样合法）：

| 行 | 含义 |
|---|---|
| `hop-done: <一行结果>` | verdict：任务做完了，正文是结论 |
| `hop-failed: <一句话原因>` | verdict：任务失败，正文是原因 |
| `hop-blocked: <一行阻塞>` | verdict：任务停在外部依赖上，正文是所等之物 |
| `handoff: <目标 role> \| <交给该 role 的一句话>` | 转手一行，可出现零到八条；`\|` 之后可缺省，缺省即把 verdict 正文当交付内容 |

行首 `#` 是注释行，空行忽略。除此之外任何不合文法的行 ⇒ 整份文件 Invalid（fail closed：零转手、零路由）。单文件上限 16 行、handoff 上限 8 条，超限同样 Invalid。CRLF 与裸 CR 先归一为 LF 再逐行分类。

报告目录由 client 在投递前创建。创建失败的那次 prompt 不带指令块，本轮按缺位情形照常结项，journal 在 `dispatch` 记录旁补一条 `warning` 记录。

turn 结束、该轮全部 `session/update` 落账之后，client 读取报告文件一次：先解析，再路由 handoff，最后删文件。结项取值：

| 报告情形 | 结果 |
|---|---|
| 文件缺位或读不到 | 维持契约前的行为：outcome 与 head 由 stopReason 与该轮末条 assistant 文本推出 |
| `hop-done: <非空>` | 报告文本作为 head；outcome 仍由 stopReason 判定，被 stopReason 判为非正常终止的那一轮保持原判；handoff 行照常路由 |
| `hop-failed: <非空>` | outcome 为 failed，报告文本同时是 head 与 fault reason，正常的 `end_turn` 也被降级；handoff 行照常路由 |
| `hop-blocked: <非空>` | outcome 与 head 取报告的阻塞正文，但任务不转手：活没干完，没有可交给下一 role 的东西 |
| Invalid（多余行、未知前缀、超限、空文件、坏 UTF-8） | outcome 为 cancelled，fault reason 以 `acp payload invalid:` 开头并写明类别与行号，head 为空，零转手；文件保留在原地，重写后同一 task 重投即可消费 |

`done|failed|blocked` 三种 verdict 由 `Outcome::Finalized` 的 `head_kind` 字段带上报文层，client 据此区分 blocked 与另两种。

handoff 的路由由 client 用该 role 已有的 server 连接发出，不冒充人类请求。单条路由失败（ACL 拒、目标 role 不在本机连接面上）只记一条 `handoff_denied`：journal 记 `handoff_denied` 事件（带 `to_role` 与拒绝原因），faults 面报同名的 fault。verdict 不删，Outcome kind 不变，其余 handoff 继续。

一条报告可以同时交给至多八个 role，每个 role 拿到属于自己那一条的正文：行内带 `| <一行>` 时收件人读那一句，缺省时读 verdict 正文（`Handoff::text_or`，`crates/onlyne-proto/src/payload.rs:29`）。

hop 记录一次转手在链条上的步深。转手链条的深度按产品初衷保持开放：协议与 server 都对 hop 计数不做上限判定，hop 只作因果记录随信封走（`crates/onlyne-proto/src/envelope.rs:331-339`、`crates/onlyne-server/src/relay.rs:640`）。一个 role 能交给谁由 `spec.toml` 的 `allowed_targets` 边决定，server 的 ACL 闸门按这些边逐条回答每一次 send（`crates/onlyne-server/src/relay.rs:379-393`）。

操作员要收束链条，改法就一条：剪掉对应的 `allowed_targets` 边再 `onlyne reload`。

每次读取都向该任务的 journal 追加一条 `payload` 记录，字段为 `task_id`、`path`、`payload_kind`（取值 `done`、`failed`、`blocked`、`invalid`、`absent` 之一）、`head`、`handoffs`（本轮可读出的转手线条数）。报告被拒时记录再多带一个 `error` 字段，写明拒绝原因与行号；缺位同样记录，账上因此能看出这一轮有没有上报。

每条值得路由的转手线在删除结项文件之前另记一条 `handoff` 记录，字段为 `task_id`、`to_role`、`head`；被拒的转手线在 client 侧落 `handoff_denied`。

结项事实经 `dispatch::on_out` 这一条通路落账：settle、`out_head`、ack 与 completion receipt 都在那里发出。每个终态任务都发 receipt，报告与末条文本都缺位的那次落一条正文为空文本的 `completion` 行。

### 本地校验动词族（`onlyne report`）

同一份文法解析器（`onlyne_proto::payload`）暴露成本地 CLI 动词，全部只读写工作区文件，不开任何 socket：

| 动词 | 行为 |
|---|---|
| `onlyne report path --task <id>` | 打印该任务的结项文件绝对路径，连同 `log:` / `events:` / `content:` 三条会话落盘路径 |
| `onlyne report check --task <id>`（或 `--path <file>`） | 合法：打印 verdict kind、head/reason 与全部 handoff 行，退出 0；非法：`onlyne: <精确原因（含行号）>` 加整份文法进 stderr，退出 2；文件不存在或读不到：stderr 报 `absent`/读失败原因，退出 2（3 只留给 socket 解析） |
| `onlyne report write --task <id> --verdict <done\|failed\|blocked> --head <text> [--handoff <role\|text>]...` | 按部件构造合法报告，临时名 + rename 原子写入，打印最终路径 |
| `onlyne report validate --text <s>`（或 `--from -` 读 stdin） | 对字符串跑同一解析器，不碰文件 |

`--workspace` 的定位与 socket 解析同一惯例：给定目录起、沿祖先目录找 `.onlyne/config.toml`；找不到就用给定目录本身。文法全文内嵌在 `onlyne report --help` 与 `onlyne report check --help` 里，装机用户不查文档也能核格式。

## 会话内容

ACP 会话没有终端：agent 是 client 持有的子进程，会话对 client 之外的进程不可见。留下的面是落盘的 journal。`<workspace>/.onlyne/logs/session-<task>.events.jsonl` 每行一个 JSON 对象，内容是该 agent 的 `session/update` 通知，加上 client 自己的 `dispatch`、`payload` 与 `turn` 记录；`<workspace>/.onlyne/logs/session-<task>.log` 是给人看的渲染件。`<workspace>/.onlyne/logs/content.index.jsonl` 每条记录一行元数据，记下它在任务 journal 里的偏移与长度，role 级的内容序号由此在 client 重启后仍可续。三个都是普通文件，谁在读写它们由本机权限决定，client 不向任何会话外的进程提供实时流。

## Windows 关停

Windows 没有 SIGTERM / SIGHUP。`tokio::signal::windows::ctrl_c` 接到现有 SIGINT 收尾路径。关停由 supervisor 在 server root 所在主机执行 `onlyne server stop`；spec 热加载走 `onlyne reload`。exec 会话子进程的杀阶梯见上一节。

`.onlyne/run/s` 在 Windows 是 marker 文件（内容 `v1:onlyne-<32hex>`），named pipe 名由路径的 lexical-absolute 小写 sha256 派生。`--socket \\.\pipe\` 原样透传。`ERROR_PIPE_BUSY` 在 CLI `--timeout` 内重试。Unix 上 AF_UNIX 仍是文件系统 UDS。
