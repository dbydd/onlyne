# Changelog

## [1.4.0] - 2026-09-21

Scope: three changes in one window. Two close session-bookkeeping holes read out of
one field report, and the third removes the mechanism the second hole lived in.
A role on the live ring logged three `connection lost` errors for a single task.
The ledger says that task completed, on all three reports, and the agents filed the
terminal write again by hand each time (`applied: true`). Nothing was lost on the
way in; the answer went out late.

The ordering hole is the client writing a `bye` to a connection that is mid-request
on itself. `adapter_socket` runs an inbound frame's handler to completion before it
answers the frame, and `Report::Complete` drives `on_out`, whose tail runs
`retire_revived` over the read-only connections a merged handoff has just answered.
The connection that filed that completion is one of them, so its bye left ahead of
its response. A plugin treats a bye as the socket dying: the pi adapter drops the
connection and rejects every request awaiting an answer, which turns a completion
the ledger already holds into a failure the agent reports again. The sweep now skips
any connection inside one of its own frames (`DispatchState::hold_frame`,
`FrameGuard`, `DispatchInner::in_frame`), and that connection is retired by its own
`detach` frame or socket end, exactly as before.

The selection hole was an idle slot nobody would ever hand work to. `control
recycle` closed the session's host resource and left the slot in the map for the
next task; the payload then went onto a session with nothing behind it while the
spawn path below stayed unreachable, so the server row sat `in_flight` and the role
still looked like it had room. That shape is unreachable now, because `reuse` is
gone and every task spawns a session of its own.

Scope, second window: the session lifecycle is rebuilt around one rule. In plugin
mode a session's state comes from the frames the mounted plugin reports and from
heartbeat liveness. Three sources competed before it. The adapter frames, a
backend probe that read a pane or a tab, and stored rows in `client.db` read as
current fact; a fourth, the client's own clocks, decided death on a schedule no
plugin had witnessed. The rule assigns each one a job. A frame from the serving
connection moves `agent`, `resource` and `host`. The client composes `delivery`
and `recovery` from its own outbound queue, and the reconcile policy with its
counters from its own tuple, before the reducer reads a beat. A probe result
answers a question and stands in for no fact. Death is one clock with three
starts — a session's birth, a lost connection, a graceful goodbye while work is
owed — cleared when a connection attaches and read by one sweep, which also
settles the task its dead session owed. The task's result left the session tuple
for a record of its own, so a session row describes a session and the task table
answers for a task. The public lifecycle left the tuple as well and is derived
where it is read.

### Breaking

- client and server: `reuse` is gone. Every task runs in a session of its own, so
  `[[client]].reuse`, `Welcome.reuse`, and `RoleInfo.reuse` are removed, together
  with the dispatcher's selection path (`family_of`, `reuse_candidate`, the
  per-slot `family`). `max_sessions` caps live sessions: a slot whose stored
  lifecycle reads `exited` holds no capacity, and a settled slot stays tracked only
  while its agent is attached — when that connection ends, the slot and its host
  resource retire. The `welcome` frame and the `roles` row no longer carry the
  field, so a new client cannot read an old server's answer and an old client
  cannot read a new one's: restart the server and every client of a role together
  on one build. The removed key also moves every `spec_hash`, which only
  `spec_diff` and the `roles` display read.
- session: the tuple a session publishes describes the session alone. `Outcome` leaves
  `Observation` and becomes `TaskState`, a value the caller hands `project`, and the
  public lifecycle leaves it too, so a reader derives `created`/`working`/`idle`/`exited`
  from the dimensions beside the task's own verdict at the point of use. `settle` loses
  its fourth argument. The fabrication both sides carried — `delivery: accepted` with
  `recovery: draining`, written for any completed task to satisfy the tuple's legality
  rule — has nothing left to satisfy, and the rule that demanded it is gone.
- client and server: `session_sync` is gone, and the client-to-server vocabulary is twelve
  verbs. A `report` whose kind is `heartbeat` is the only carrier of session state: a beat
  with no projection is liveness alone, an accepted publish passes the same
  `(generation, seq)` gate as every state write before it, and the mirrored row keeps
  `desired` empty. A client and a server from different sides of this change cannot read
  each other's state on that path, so restart them together on one build.
- adapter: a plugin's `report.heartbeat` carries an observation of the session's own
  dimensions. The `outcome` and `public` keys are gone from it, and the host discards
  whatever the body claims for `delivery`, `recovery`, `generation_live`, `isolate_after`,
  `terminate_after` and `mismatch_count`, which are the client's own. A body without the
  removed keys decodes, so an older plugin keeps mounting and is answered the same way.
- store: the client database moves to schema marker 2 and the server's to 3. The
  `sessions` row loses `public_lifecycle`, and the client gains a `task` table —
  `task_id`, `kind`, `parent_task`, `hop`, `attempt`, `task_state`, `opened_at`,
  `settled_at` — which holds the task's result where the session row used to. A database
  written by the previous layout is refused outright with `onlyne: unsupported schema;
  v1.0.0 does not migrate`, so a workspace on this tree migrates its own data or drops it.
- plugin: a turn that ends without `onlyne_complete` no longer completes itself. The
  settle window reported every task with a turn behind it as `done` two seconds after the
  turn ended, which settled the task and retired the session. An active task now gets the
  design's reinforcing prompt — the assignment again, its task text and handoff lines
  included — bounded by `idleReminders` in `.pi/onlyne.json`, default 2, and the idle
  after the bound reports the task `failed` and ends the session. An errored turn still
  reports `failed` at once. A role whose agent never calls the tool now sees its work fail
  rather than silently succeed.

### Changed

- server and tui: `RoleInfo` carries `queued`, the exact number of deliveries
  waiting in one role's inbox, counted by the server
  (`ServerLedger::queued_count_for`) rather than read off a capped page. It is
  the depth `relay::pull` would hand a session of that role, `note` rows excluded
  as `pull` excludes them. A row from a server that predates the field omits the
  key and reads as nothing waiting. Page 1 prints it beside the role's capacity
  and the role's map box counts it in its third interior row.
- tui: the role map is a function of the session set, not of the order the server
  happened to answer in or of the clock. `visible_sessions` and `live_sessions`
  sort on `(role, task_id)`, so the role boxes, the page-2 graph table and the
  row its cursor highlights agree and hold still across a refresh; the server's
  own `ORDER BY updated_at DESC` is untouched. `box_width` measures a fixed
  number of cells for a session row's age and takes the widest row of the whole
  set rather than the first two, so a heartbeat or a longer age no longer resizes
  every box and re-routes every hop.
- config: a key no field declares is ignored, and the parse still succeeds. Each
  ignored key is named once by `onlyne-config`'s `keys` walker, which reads the
  same generated JSON Schema `config-schema` writes, so a path like
  `server.extra`, `client[0].x`, or `orca.extra` reaches the loader's `tracing`
  warning with no list to maintain. A wrong value for a declared key still refuses
  the load with its line number, and the two generated schemas shed
  `additionalProperties`.
- config: `ServerSection.crate` carries `#[serde(default)]`. A `[server]` table
  without a `crate` list loads, which is the shape `onlyne server generate` and the
  relocate e2e read; before this it failed with `missing field crate`.
- client: a plugin heartbeat moves `agent`, `resource` and `host`, and the client composes
  the rest of the tuple before the reducer reads it (`compose_observation`). A beat
  claiming `delivery: none` cannot clear an intent the server has not receipted, and a
  beat cannot reset the reconcile ladder or its counters: `isolate_after`,
  `terminate_after`, `mismatch_count` and `generation_live` come from the client's own
  tuple, where the plugin sends constants.
- client: the receipt reaches the reducer. An accepted answer on the outbound queue —
  `IntentResult::Accepted`, which the flusher had parsed and dropped — now feeds
  `IntentReceipt` for the session its intent names, so `DeliveryState` records `accepted`
  once the server answers and a completed task exits through `Done` beside `Accepted`.
  The routing accepts exactly two payload shapes, a `completion` envelope and a
  `report.complete`, because a receipt fed for any accepted op that merely names a task
  would close a completion's drain on the strength of an ack.
- client: death is one clock with three starts and one clear. A slot is born with the
  window running, a connection end restarts it, a graceful detach past the point the
  session still owes work restarts it, and an attaching connection clears it. The sweep
  reads that stamp and takes every session, task-bound included; at expiry it feeds the
  agent-gone event, settles the task `failed`, and closes the resource with the reason the
  task's own state earns. `session_alive`, the attach-and-probe liveness check, is gone
  with its last caller.
- client: the sweep reads silence as well. A plugin that holds its connection and stops
  beating opens the same window once its last accepted frame is older than
  `HEARTBEAT_INTERVAL` times a margin, guarded on an attached transport, a bound task and
  an unsettled verdict, because the pi plugin stops beating between tasks by design and a
  quiet session with nothing owed is an agent waiting for work.
- client: a mount that follows a drop rebases the watermark, so a reporter that reconnects
  inside the grace window lands its next frame. The version a beat carries is the
  generation the session's tuple holds beside the reporter's own sequence; the plugin's
  generation field is read nowhere on that path. The rebase moves the generation with the
  content, because the reducer's no-op detection compares the dimensions without the
  version and a watermark-only event reads as a replay.
- client: a state frame is applied only when its sender is the connection the client bound
  to that session (`serves_session`). A connection the client holds read-only is answered
  `ok` and its claims about state reach the log alone; a second verdict for a task whose
  record is settled builds no receipt and releases no binding, which stops a zombie
  completion from clearing the live session's delivery handle.
- cli and server: `onlyne sessions --fresh --task T` asks T's owning client to probe its
  plugin and answers what that probe produced. The wait lives inside the read's own
  `--timeout` less a reserve, the answer marks each row `probed`, `offline` or
  `unanswered`, and a read without the flag sends no frame and waits on nothing.
- config and server: a role template carries every dot-directory it holds, minus `.onlyne`
  and `.git`. `.pi` was the one exception to dot-directory pruning, which kept a template
  from shipping the project-local configuration of another agent runtime; a template can
  now carry `.omp/`, an opencode tree, or whatever comes next. The `../`-form
  `agent_package` reference generalizes with it: any `settings.json` sitting directly under
  a top-level dot-directory receives it, because a project `packages` path resolves against
  the directory holding that settings file.
- config and server: a role named `_supervisor` reaches every registered role by default,
  and `onlyne_config::SUPERVISOR_ROLE` is the one spelling of that name. Its own
  `allowed_targets` is the only gate: empty reaches every registered role, a non-empty list
  narrows the reach to what it names, and the receiver's `allowed_senders` is not consulted
  for that sender. Every other pair keeps the two-sided rule, and the reverse direction is
  untouched, so an operator's inbox stays as narrow as the spec says. This is the repair
  path: the role that needs repairing is the one whose own list would have denied the
  supervisor.
- client: `idle_waiting` is reachable. The composition behind a plugin heartbeat derives
  it — an idle agent, a task still open, and no accepted delivery — which is the reducer's
  own turn-end rule with no producer until now. The label clears the moment a beat reports
  the turn running again, so the frame that says work resumed is not refused as illegal,
  and a session with no task record is left alone.
- tui: the role named `_supervisor` is not drawn. One filter on the role registry
  (`Snapshot::visible_roles`) removes its box, every hop in either direction, its seat for
  the cursor, its role-list row and its sessions. A fault, ledger or history row that names
  it still prints the name: those record messages that named a principal, and hiding them
  would erase the operator's own audit trail. The `e` key and the hidden-by-default control
  spokes it revealed are gone with it, so an aggregate role's hops draw like any other.

### Fixed

- client: the answer to a plugin's own report leaves before any bye that report
  triggers (`crates/onlyne-client/src/adapter_socket.rs`, `crates/onlyne-client/src/
  dispatch.rs`, `retire_revived`). One task's terminal write is now reported once.
- client: no path answers a task question from a stored session row. The close reason, the
  capacity count, the "this role already finished this task" check and the stall report
  read the task's own record and the derived projection.
- client: the grace sweep leaves a slot the client holds read-only alone, and feeds the id
  its close reason reads. A ghost whose task a newer session took can no longer push the
  live session's mirror to `exited` or release its in-flight delivery row.
- client: a session whose plugin never mounted has a clock. It held a slot and its host
  resource until its task settled by some other path, and for a plugin that never arrived
  there was no other path.
- session: the reducer's `AdoptNewGeneration` and `Supersede` have a producer, and the
  rebase they exist for is reachable from a re-mount.
- store: both listing reads stop spilling a sorter into a temporary file. Their
  `ORDER BY … DESC, rowid DESC` could never be satisfied by an index — SQLite refuses
  `rowid` as an index column — so every read scanned the table and materialized the order,
  and a read on a timer turned that into megabytes per second of writes nothing asked for.
  An ascending index on `sessions(updated_at)` and on `ledger(enqueued_at)` is read
  backwards instead, which satisfies the whole order, and a listing filtered by `state`
  keeps using `ledger_state_enqueued_idx` the same way. The orders themselves are
  unchanged.

### Tests

- client: `each_task_gets_its_own_session_and_max_sessions_caps_the_live_ones`
  replaces `session_reuse_and_capacity_capping`, and the cases that asserted an
  idle slot keeps work were rewritten to the one-session-per-task shape:
  `a_recycled_slot_is_gone_and_the_next_task_spawns_its_own`,
  `a_settled_session_whose_plugin_left_is_retired`,
  `a_parked_agent_serves_the_session_it_claimed`,
  `an_agent_that_reconnects_inside_the_window_keeps_its_session`, and
  `the_second_task_gets_its_own_session_and_connection`
  (`crates/onlyne-client/tests/scenarios.rs`).
- config: `an_unknown_spec_key_loads_and_is_named`,
  `an_unknown_client_key_loads_and_is_named`, and
  `the_guard_alias_is_not_reported_as_ignored` in
  `crates/onlyne-config/tests/config_contract.rs`, plus
  `no_shipped_config_key_goes_unrecognized`, which holds the example spec, the
  fixtures, and `templates/**/config.toml` to the schema
  (`crates/onlyne-config/tests/spec_example.rs`). The generated schemas are held
  to their new shape by `the_published_client_schema_carries_acp`, which asserts
  the client schema declares no `additionalProperties`.
- client: the rebuilt lifecycle holds one case per rule —
  `a_re_mounting_plugin_inside_the_grace_has_its_next_heartbeat_applied`,
  `a_state_frame_from_a_connection_held_read_only_leaves_the_tuple_alone`,
  `a_session_whose_plugin_stops_beating_dies_when_the_window_expires`,
  `a_session_whose_plugin_never_mounts_retires_past_the_grace`,
  `the_reconnect_grace_does_not_take_a_slot_the_client_holds_read_only`, and
  `a_session_that_died_at_the_grace_window_settles_the_task_it_owed`
  (`crates/onlyne-client/tests/scenarios/reconnect.rs`, and
  `crates/onlyne-client/src/session/dispatch/{reports,retire}/tests.rs`).
- client: `a_beat_disowning_the_drain_leaves_the_open_intent_in_place`,
  `a_beat_cannot_reset_the_reconcile_policy_or_its_counters`, and
  `heartbeat_junk_in_the_client_dimensions_writes_none_of_it` pin the ownership split at
  the plugin door, and
  `an_accepted_completion_intent_exits_the_session_without_closing_its_resource` pins the
  receipt path.
- session: the reducer's tables were rebuilt around the five dimensions
  (`crates/onlyne-session/src/lifecycle/tests.rs`), and
  `a_done_task_with_its_intent_still_in_flight_stays_working` is the shape the removed
  legality rule refused.
- tui: `a_permuted_session_order_renders_the_same_picture` renders one logical snapshot
  twice with `sessions` reversed and asserts byte equality, and holds `box_width` still
  under the same permutation (`crates/onlyne-tui/src/ui.rs`).
- proto: `the_two_heartbeat_shapes_write_only_their_own_keys` holds the publish beat and
  the liveness beat to their own key sets, so an older plugin's bytes keep decoding.
- config, client, tui and plugin, for the second window of this release:
  `supervisor_default_reaches_a_role_that_does_not_admit_it` and
  `supervisor_explicit_targets_narrow_its_reach`
  (`crates/onlyne-config/tests/acl_table.rs`);
  `an_idle_beat_over_an_open_task_is_composed_idle_waiting`
  (`crates/onlyne-client/src/session/dispatch/reports/tests.rs`);
  `a_registered_supervisor_draws_nothing`, which compares a snapshot carrying the role
  against the same snapshot with it stripped, page by page (`crates/onlyne-tui/src/ui.rs`);
  and `an idle without a completion is reminded, the idle past the bound fails the task,
  and a completed turn is not reminded` (`plugins/onlyne-agent-pi/src/agent.test.mjs`).

### Check on this tree

Run 2026-09-22, after the supervisor, TUI and idle-ladder slices: `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo test --workspace` pass, the last at 1009 cases across 68 result blocks with 0
failures and 1 ignored (`herdr_live_probe`); `node --test src/*.test.mjs` in
`plugins/onlyne-agent-pi` reports 97 pass and 0 fail.

Run 2026-09-21, after the lifecycle rebuild: the same gate at 1006 cases across 68 result
blocks with 0 failures and 1 ignored.

Run 2026-09-20, earlier in the same window: `cargo fmt --all --check`,
`cargo clippy --workspace --all-targets -j 4 -- -D warnings`, and
`cargo test --workspace --no-fail-fast -j 4 --lib --tests`
pass, the last at 975 cases across 50 suites with 0 failures and 1 ignored
(`herdr_live_probe`); `cd proofs && lake build` completes with 7 jobs. Both e2e
runs below used `BIN_DIR=target/release` on this tree.

`crates/onlyne-testkit/e2e/local-task.sh` passes. `reconnect-requeue.sh` fails on
this tree with `acked=1` and on its parent 898de2d with `acked=2`, so the red line
predates this release. That fixture hands three tasks to one `onlyne-agent-fake`
process, and one process serves one session: the other sessions have no transport,
so their tasks never settle. A role that runs more than one concurrent session needs
one agent per session, and the fixture has to mount them.

`crates/onlyne-testkit/e2e/local-task.sh` passes on this tree too, run 2026-09-21 with
`BIN_DIR=target/debug`. The `reconnect-requeue.sh` paragraph above dates from 2026-09-20
and the lifecycle rebuild did not re-run that fixture; its own proof lives in the
workspace tests named under `### Tests`.

### Documentation

- `docs/operations.md`, 「会话残影与属主判定」: the bye exception and why an answer
  must precede it.
- `docs/operations.md`, 「配置加载」: ignored keys warn once, declared keys with
  wrong values still refuse, and a client and its server restart together on one
  build.
- `docs/v1-ARCHITECTURE.md` §5: the client task path no longer selects among idle
  sessions; it spawns a session per task.
- `AGENTS.md` §6 and §8: the client-to-server vocabulary is twelve verbs, `report`'s
  heartbeat variant is the only state carrier, and the schema markers read 2 and 3.
- `docs/operations.md`: the `onlyne sessions` entry carries the `--fresh` bound, its
  three answer markers and its `--task` requirement; the retired-scan rule reads the
  derived lifecycle beside the task's own record.
- `docs/v1-ARCHITECTURE.md`, `## Session lifecycle`: the dimension table lists the five
  session dimensions, `TaskState` as an input and the public view as a derived reading;
  `## CLI verbs and flags` names the fresh read.
- `crates/onlyne-adapter/PROTOCOL.md`: the heartbeat example carries the session's own
  observation, and a beat's claims about the client's dimensions are discarded at the
  door.
- `plugins/onlyne-agent-pi/README.md` and `README.zh.md`: the plugin reports `agent`,
  `resource` and `host`, and the completion report carries the task's verdict.

Wire format: `Welcome` and `RoleInfo` lose `reuse`; `RoleInfo` gains `queued`,
`SessionRow` gains `fresh` and `QuerySessionsArgs` gains `fresh_wait_ms`; the
`session_sync` op is gone and `Report::Heartbeat` gains `session_id` and `projection`,
both skipped when unset, so a bare liveness beat keeps its bytes. The observed object a
plugin sends loses its `outcome` and `public` keys. Both proto schemas are regenerated.
The spec and client config schemas lose `additionalProperties`, and their fixtures move
with them.

## [1.3.1] - 2026-09-20

Scope: one bug class in the client's accept path. The server re-offers a delivery
row whose ack has not landed — a link flap, an adoption requeue, an operator
`repair retry` — and a completion still in the durable intent queue when that
happens arrives after the row it answers. Such a redelivery reached
`accept_delivery` looking like new work, and the dispatcher's `reuse` branch
stages new work on whichever session sits idle: it looks for a session in the
same family, finds none, and takes any idle slot. So a task belonging to one
chain ran inside another conversation, with that agent's context and its
plugin's process-memory state, and its second answer travelled against the
ledger row the first answer had already settled. The existing
`dispatch::tests::a_read_only_slot_never_holds_the_handle_of_the_task_it_lost`
case already names the symptom in its own comment; that fix moved the delivery
handle to the right slot, and the execution half stayed open.

The guard reads the durable record this role already owns. `settle` writes the
terminal outcome the agent filed into `client.db`, so a session row reading
`Done` means this role answered for that task id once. The predicate is
`DispatchState::task_completed_here`, and `accept_delivery` applies it ahead of
the capacity gate and the `accept_new` gate: the row is acked with
`accepted = true` and reason `task already completed by this role`, nothing is
staged, and no capacity is spent. Acking is what ends the requeue loop, so the
row settles whatever the link state.

`Done` is the only outcome that closes the door. A session killed or crashed
mid-flight ends without a `Done`, and `requeue_max_attempts`, `repair_retry`,
and `control retry` exist to re-offer exactly those rows, so a failed or
cancelled task stays retryable. The second test pins that boundary.

The release stops a second bleeding point, found by running the workspace suite
1.3.0's own gate had skipped. `onlyne server generate` on a fresh tree wrote
`config.toml` and the role key and left every template file out: the write loop
asked the overwrite guard's predicate `differs_from_render` whether to write
(`crates/onlyne-server/src/generate.rs`), and that predicate answers `false` for
a path that does not exist, which is correct for the guard — a missing file is
nobody's hand edit — and inverted for the writer, where a missing file is exactly
the copy to place. Nine cases in `crates/onlyne-server/tests/generate.rs` were
red on the shipped 1.3.0 tree, reproduced on a detached worktree at the release
commit, and the release note's check covered `onlyne-client` and `onlyne-config`
alone. The writer now asks its own predicate, `needs_write`.

### Fixed

- client: a redelivery of a task this role completed is acked and runs nowhere
  (`crates/onlyne-client/src/runloop.rs`, `accept_delivery`).
- client: `DispatchState::task_completed_here` (`crates/onlyne-client/src/dispatch.rs`)
  answers from the stored session row through `stored_close_reason`, so the
  decision survives a client restart with the workspace.
- server: `onlyne server generate` writes every template file into a fresh
  workspace, and a no-op rerun still leaves each existing file's bytes and mtime
  untouched (`crates/onlyne-server/src/generate.rs`, `needs_write`). This closes
  the nine red cases in `crates/onlyne-server/tests/generate.rs`, among them
  delivery case 9's generate-plus-relocate path.
- client test lint: `witnessed.try_recv().is_err()` replaces a
  `matches!(.., Err(_))` that clippy 1.98 rejects
  (`crates/onlyne-client/tests/scenarios.rs`). No behavior moves.

### Check on this tree

Run 2026-09-20: `cargo fmt --all --check`, `cargo clippy --workspace
--all-targets -j 4 -- -D warnings`, and `cargo test --workspace --no-fail-fast
-j 4 --lib --tests` pass, the last at 970 cases across 50 suites with 0 failures
and 1 ignored (`herdr_live_probe`), in 144 seconds. This is the full-workspace
gate, `onlyne-server`'s generate suite included, which is the coverage the 1.3.0
note did not claim.

### Added

- client tests: `a_redelivered_finished_task_is_acked_and_runs_nowhere` and
  `a_task_ended_without_a_completion_stays_eligible_for_its_retry`
  (`crates/onlyne-client/src/runloop.rs`).

### Documentation

- `docs/operations.md`, 「投递与重投」: the operator-facing rule, including the
  consequence that `repair retry` on a row whose task is already `Done` for this
  role is answered with an ack and no execution. Re-running finished work takes
  a new task via `onlyne send`.

### Not in this release

Three neighbouring defects stay open, each needing a wider change than a patch:
the `reuse` fallback picks any idle session once family preference misses, so
delivery is order-dependent across unrelated chains; `park_transport` holds one
parked connection, so a second unnamed mount silently evicts the first; and
`slot_key_serving_task` returns a read-only slot when it is the only one
matching, which leaves a payload on a connection that can never receive it.

Wire format: unchanged. No `onlyne-proto`, `onlyne-config`, or generated schema
file moves in this release.

### Receipt

All nineteen crates are on crates.io at 1.3.1, published 2026-09-20, none yanked,
in the dependency order `onlyne-acp`, `onlyne-config`, `onlyne-frame`,
`onlyne-layout`, `onlyne-proto`, `onlyne-adapter`, `onlyne-cli`, `onlyne-net`,
`onlyne-session`, `onlyne-gateway-feishu`, `onlyne-gateway-qqbot`,
`onlyne-gateway-telegram`, `onlyne-gateway-weixin`, `onlyne-store`,
`onlyne-testkit`, `onlyne-client`, `onlyne-gateway`, `onlyne-server`,
`onlyne-tui`, each through `cargo publish --locked -p onlyne-<crate>` at tag
`v1.3.1` from a clean worktree, driven by `scripts/publish.py`. Seventeen went out
on the first attempt, `onlyne-acp` and `onlyne-proto` on the second. Every crate
carried cargo's packaging sandbox build; the run needed neither `--allow-dirty`
nor `--no-verify`. The whole publish closed in 362 seconds.

The local install was refreshed from the registry rather than from the tree, which
proves the published artifacts resolve on their own: `cargo install --locked
onlyne-cli onlyne-server onlyne-client onlyne-gateway onlyne-tui onlyne-testkit`
built in 231 seconds and replaced all seven executables in `~/.cargo/bin`.
`onlyne version` answers `{"onlyne-cli":"1.3.1","protocol":1}` with
`onlyne-server`, `onlyne-client`, and `onlyne-gateway` resolved under
`~/.cargo/bin`. A role already running keeps the image it started with, so the
redelivery guard lands on the next restart of each `onlyne-client`.

## [1.3.0] - 2026-09-20

Scope: the core stops describing a running turn as bounded, and a session whose
plugin never comes back ends. `[client.timeout]` carried a `running_ms` key
documented as bounding one running task, and the server projected it into the
`welcome` reply beside its two siblings. The key carries no behavior: no reader
in `onlyne-client` or `onlyne-session` consults a running-task clock, and the ACP
transport already states the working rule (`onlyne-acp` waits on the agent and
lets the agent end its own turn). This round cuts the key from the parser, the
wire, the generated schema, the shipped examples, and the plan. What survives is
the split the code already keeps: `ready_ms` covers the handoff window before an
agent starts work, `idle_ms` covers a session nobody claims, and the
detection-only watches (`stall_report_secs`, `stale_watch_secs`,
`heartbeat_grace_secs`) record a fault and leave the row `working`. The second
half gives the client a bounded promise where it had an unbounded one: a plugin
connection that ends without a `detach` frame keeps its session for
`[client] reconnect_grace_secs` (60 seconds) and no longer, and a session that a
retry already answers stops accepting what its returning agent says, holding
those lines for the merged handoff instead. The third gives
`onlyne server generate` a per-file overwrite guard, so a workspace an operator
customized survives a rerun.

Check on this tree, run 2026-09-20: `cargo fmt --all`, `cargo check --workspace
--all-targets`, and `cargo test --no-fail-fast -p onlyne-client -p onlyne-config
--lib --tests` pass, the last at 208 cases with 0 failures. Every other member
keeps the 2026-09-19 full-workspace count below. On the field root that started
this round, a role whose `pi` process was killed outside its session now retires
the session the client tracked for it, which is the behavior the round adds.

### Removed

- config: `Timeouts::running_ms`, its default helper, and its entry in the
  unknown-key name list. `[client.timeout]` now reads `ready_ms` and `idle_ms`.
  The struct keeps `deny_unknown_fields`, so a `spec.toml` still carrying
  `running_ms` stops at parse with that key named.
- proto: `Welcome::timeout_running_ms`. The field was `Option` with
  `skip_serializing_if`, so an older server sending it lands as an ignored key
  (`Welcome` declares no `deny_unknown_fields`), and an older client reading a
  reply without it gets `None`.
- server: the `router` line that copied `entry.timeout.running_ms` into the
  `hello` reply.
- docs: the `running_ms` entry in the plan's `[[client]]` table, the
  `running_ms` comment and value in the four `[client.timeout]` blocks of
  `.onlyne.example/spec.toml`, and the `timeout` line `onlyne-client init` prints
  into a pasted `[[client]]` fragment. The surviving comments name what actually
  touches those two keys: the server projects them into the `hello` reply.

### Changed

- server: `onlyne server generate` guards each template file by content. A file
  missing from the target gets created. A file whose bytes already match this
  render is left alone, so a rerun following a `spec.toml` edit writes nothing it
  does not have to, and the untouched file keeps its mtime. A file holding other
  bytes is a hand edit, and the run stops before writing anything with
  `onlyne: refusing to overwrite <path>; pass --force` at exit 4. `--force`
  replaces those files. `.onlyne/config.toml` stays a derived artifact and
  refreshes each run; `role.key` is written when absent; `client.db`, `run/`, and
  `logs/` stay untouched either way.
- server: `onlyne server init` on a root that already has a `spec.toml` prints
  the same refusal wording, which is the message §6 of `AGENTS.md` has
  documented throughout.
- client: a plugin connection that returns for a session another connection
  already serves is held. Concurrency decides: a mount finding its session held
  by another transport, or its task answered from a slot of its own, never joins
  `transports`, so it is handed no assignment, delivery, or note. The connection
  the client waited behind is what promotes it, which keeps an agent that redials
  over a socket the client has not yet seen die in the session it owns.
- client: what a held connection sends is buffered instead of queued, answered
  `{"queued": true, "held": true}`, and leaves at its task's completion as one
  envelope per downstream role, each line marked `[retry]` or `[zombie]` so the
  recipient reads the two accounts in one message. The held connection is then
  sent `bye`, and a slot that lost its task to a newer session retires as
  `CloseReason::Replaced`. The task's own account stays the single one its
  completion pays.
- client: four task-to-slot lookups (`attach_msg_id`, `push_assign_ack`,
  `on_out`, `release_locked`) name the slot serving the task. Two slots on one
  task let `HashMap` order decide which one held the message id, and that is how
  a retry's acknowledgement could reach the session which had lost the task.

### Added

- config: `[client] reconnect_grace_secs`, defaulting to 60 and disabled at `0`.
  It bounds the promise that a session keeps its slot, its projected `idle` row,
  and its live host resource while its plugin reconnects. `onlyne-client init`
  writes the key commented beside `backend`, and the published client schema
  carries it with the parser's own default.
- client: `DispatchState::retire_dropped_ghosts`, driven from the readiness tick
  beside `reclaim_exited_resources` and `scan_stalls`. A connection ending
  without a `detach` frame starts the clock; a mount that takes the session back
  clears it; a session past the window with no task bound retires through the
  existing idle path, with the reason its settled task earned and `Fault` where
  it earned none. A session still bound to a task belongs to lifecycle, and the
  retry that answers it ends it through the merge above. `docs/operations.md`
  states the boundary, including the shape this window leaves alone: a ghost
  whose retry never arrives stays with the stall and heartbeat watches.

All nineteen crates move to 1.3.0. Four carry code: `onlyne-config` loses
`Timeouts::running_ms` and gains the reconnect knob with its schema entry,
`onlyne-proto` loses `Welcome::timeout_running_ms`, `onlyne-server` loses the
router line that projected it and guards each generated file by content, and
`onlyne-client` runs the grace window, the held connection, and the merged
handoff. The other fifteen — `onlyne-frame`, `onlyne-layout`, `onlyne-store`,
`onlyne-session`, `onlyne-net`, `onlyne-adapter`, `onlyne-acp`, `onlyne-cli`,
`onlyne-tui`, `onlyne-testkit`, `onlyne-gateway`, and the four gateway plugins —
move on their internal path-dependency floors, so each manifest stays publishable
on its own.

Receipt: all nineteen crates are on crates.io at 1.3.0, published 2026-09-20,
none yanked, in the dependency order `onlyne-acp`, `onlyne-config`,
`onlyne-frame`, `onlyne-layout`, `onlyne-proto`, `onlyne-adapter`, `onlyne-cli`,
`onlyne-net`, `onlyne-session`, `onlyne-gateway-feishu`, `onlyne-gateway-qqbot`,
`onlyne-gateway-telegram`, `onlyne-gateway-weixin`, `onlyne-store`,
`onlyne-testkit`, `onlyne-client`, `onlyne-gateway`, `onlyne-server`,
`onlyne-tui`, each through `cargo publish --locked -p onlyne-<crate>` at tag
`v1.3.0` from a clean worktree. Every crate went out on its first attempt, zero
retries, and all nineteen carried the packaging sandbox build cargo runs by
default — the tree needed neither `--allow-dirty` nor `--no-verify`. Each publish
closed on its registry availability poll rather than on a skip, so the build of
every crate after it had its dependencies' 1.3.0 versions in the registry to
resolve against.

The local install was refreshed from the same tree: `cargo install --force
--locked --path crates/<crate>` for the six binary members, which leaves seven
executables at 1.3.0 in `~/.cargo/bin` (`onlyne`, `onlyne-server`,
`onlyne-client`, `onlyne-gateway`, `onlyne-tui`, `onlyne-agent-fake`,
`onlyne-gateway-fake`). One of those six died on its way through the registry
index refresh — `LibreSSL SSL_connect: SSL_ERROR_SYSCALL` against
`index.crates.io:443` — and the retry for `onlyne-testkit` landed it, which is
the single hiccup of the release. `onlyne version` answers
`{"onlyne-cli":"1.3.0","protocol":1}` with all three daemons resolved under
`~/.cargo/bin`, and `onlyne schema client` prints 156 lines against 1.2.2's 149:
the seven-line difference is `reconnect_grace_secs`.

## [1.2.2] - 2026-09-19

Scope: the acp closing report grows a routing vocabulary. The closing report is
the one file that ends a task, as before: a verdict line, plus zero to eight
`handoff:` lines naming another role. A handoff names another role for the work
to travel to, and the client that reads the file sends it there. The grammar
lives in one place, `onlyne_proto::payload`, so the prompt an agent reads, the
`onlyne report check` an operator runs, and the client that settles the turn
parse the same bytes. A new `onlyne report` verb family lets any author validate
that file before the turn stops, with no daemon and no socket. The
discoverability pass that came with it puts the answer inside the help surfaces:
flag ranges, key sets, defaults, and the dead ends, so an agent or operator
reading `--help` needs no other file.

Gate on the tree at `86d34ff` plus this round's edits, run 2026-09-19:
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -D warnings`,
and `cargo test --workspace` on the bumped 1.2.2 manifests give 962 passed, 0
failed, 1 ignored across 68 suites. That count sits eleven above the gate that
closed the payload-v2 work.
The eleven new cases are six CLI cases for the `complete` head sources (the text
the task reports, which the row keeps in `out_head`) and the `schema` surface,
three for the `--backend-ref` shapes, one client launch-path case for a secret
name the environment leaves unset, and one `wait-ready` case for its exit code.
Fake-backend e2e runs fifteen cases at exit 0 — 1 through 7, 9, 12, and 14
through 19 — including `acp-payload-v2.sh` (case 19, new: two routed relays
(handoffs the client delivered), one refused relay, one blocked report, one
invalid report rewritten), `acp-session.sh`, and `running-lights.sh`.

### Added

- proto: `onlyne_proto::payload` owns the payload-v2 grammar (the shape a report
  file follows) as one authority. `parse` reads one report file into
  `PayloadV2::{Done, Failed, Blocked, Invalid}` with its `Handoff` lines.
  `GRAMMAR_V2` is the prose the prompt and the CLI help both print.
  `MAX_REPORT_LINES` caps a file at 16 lines, and `MAX_REPORT_HANDOFFS` caps it
  at 8 handoff lines. Verdict prefixes are `hop-done:`, `hop-failed:`, and
  `hop-blocked:`. A handoff line is `handoff: <role>` with an optional
  `| <one line>`. Lines starting with `#` and blank lines are skipped, and CRLF
  and a lone CR normalize before the first line is classified. Every rejection
  names the physical line number the author will re-open. An unreadable file
  yields `Invalid` carrying zero handoffs, so a report this client cannot trust
  routes nothing. Seventeen unit cases.
- session: the `acp` backend records each handoff line in the task journal
  before it deletes the report file, and `head_kind` keeps a self-reported block
  apart from a broken turn. A `hop-blocked:` report settles `Failed` and carries
  the agent's own reason. An invalid report settles `Cancelled` and leaves the
  file on disk, so a rewrite is enough. `emit_handoffs` runs ahead of
  `remove_file`, and the parse failure returns before the delete.
- client: `onlyne_client::handoff` puts one handoff line on the wire as a
  `MsgKind::Task` envelope over the role's existing link. The envelope is the
  child of the settled task, one hop deeper (a hop is one step along the chain
  of handed-on tasks), and its body carries the `handoff: ` prefix. The whole
  set runs under a six-second budget. `acl_denied` and `unknown_role` land as
  `handoff_denied` events beside one fault row in `client.db`, and the verdict
  and the outcome kind stay untouched. A link that cannot answer at all leaves
  the envelope to the durable intent queue, where every other outbound frame of
  that role already waits.
- cli: `onlyne report path`, `check`, `write`, and `validate`. `path` prints the
  report file and the three journal surfaces of one task. `check` parses a file
  and prints the verdict and every handoff, or the exact line it broke on plus
  the whole grammar. `write` builds a valid report from `--verdict`, `--head`,
  and repeatable `--handoff` parts, then renames it into place atomically.
  `validate` reads a string or standard input with no file touched. Exit codes
  follow the contract: 0 success, 1 io, 2 for an invalid, absent, or unreadable
  report and for refused arguments, with 3 reserved for socket resolution.
  `path` pins the four surfaces an agent or operator needs to find after a turn,
  and it refuses a `--task` that is not one bare file name. Eight integration
  cases run the shipped binary.
- client: the `$NAME` spelling now reaches the running daemon. `cert_pin`,
  `key_path`, and `[server] host` read from `config.toml` through
  `Env::current()` at the top of `run`, so a workspace can carry an environment
  variable name in place of a pin. The gateway plugins already use that same
  idiom for platform tokens. A name the environment holds no value for stops the
  launch with exit 1 and names both the field and the variable:
  `onlyne-client: missing secret $ONLYNE_CERT for cert_pin; set the environment
  variable`. The resolver, the `Env` readers, and their tests were in the tree
  ahead of this, and `onlyne-client run` is the caller that was missing. One
  binary-target case runs the launch against a workspace whose pin names an
  unset variable and asserts the refusal text.
- cli: `--backend-ref` on `repair adopt` and `repair rebind` carries an object.
  A spelling that parses as JSON travels as that value. The pane references the
  client matches on (`dispatch.rs` reads `backend_ref.get("id")`) now have a CLI
  spelling: `--backend-ref '{"id":"p-7"}'`. Any other text travels as one JSON
  string, and an omitted flag travels as null. Three unit cases cover the
  three shapes.
- cli: `onlyne schema client|spec [--pretty]` prints the generated JSON Schema
  for one config surface — `<workspace>/.onlyne/config.toml` for `client`,
  `<server-root>/.onlyne/spec.toml` for `spec` — and it reads that schema from
  the same compile-time document `onlyne-config` validates against. The answer
  is local: no socket, no daemon.
  `onlyne completions <bash|elvish|fish|powershell|zsh>` writes the shell's
  completion script for the whole vocabulary. Four integration cases cover the
  two target documents, the longer `--pretty` rendering, and the exit-2 refusal
  of an unknown target.
- testkit: e2e case 19, `crates/onlyne-testkit/e2e/acp-payload-v2.sh`, drives a
  real server, an acp-role client, and a fake recipient through the four endings
  a report can have. It asserts the routed children's `parent_task`, `hop + 1`,
  literal body prefix, and completion receipts. `e2e/acp-agent.py` gained one
  optional `--caller-report-marker` selector; with the flag absent the scripted
  agent behaves byte for byte as before.
- skills: `skills/onlyne-role-payload-v2/SKILL.md` teaches an acp role the
  grammar, the path lookup, the self-check before it stops, and what a
  refusal costs.

### Changed

- proto, cli: the two repair flags the server never read are gone. `RepairFail`
  carries `task_id` and `reason`. `RepairAdopt` carries `task_id`, `backend`,
  `backend_ref`, and `reason`, and it keeps the session id the row already holds
  — moving a task to another session is `rebind`'s job, which writes the id
  beside the generation bump. `onlyne repair fail --notify` and
  `onlyne repair adopt --session-id` no longer exist. A supervisor that wants a
  role to hear about a settlement sends it
  (`onlyne send --from <supervisor> --to <role> --text ...`) or watches the
  `ledger_state` events. Both structs deserialize with serde `default`, so an
  older script line that passes either name lands as an ignored key.
- cli: `onlyne complete --head-from` carries a default. The flag accepts `local`
  and `ledger`, and it now reads `local` when omitted, the shape nearly every
  caller wants. `--text` became optional, and the `local` branch alone requires
  it, since the `ledger` branch takes the head from the row's own `out_head`.
  Missing both answers `onlyne: --text is required with --head-from local` and
  exits 2, the same refusal style as the rest of the family.
- layout: one owner now spells the per-task file names. `RoleWorkspace` gained
  `out_dir`, `report_path`, `session_log_path`, `session_events_path`, and
  `content_index_path`, beside `CONTENT_INDEX_FILE_NAME`. `onlyne-session` takes
  the layout edge, and `backend/acp.rs`, `backend/exec.rs`, `content.rs`, and
  the CLI's `report path` verb all read those accessors. The duplicated
  `CONTENT_INDEX_RELATIVE` and `REPORT_DIR_RELATIVE` constants are gone, so a
  workspace `generate` writes, a session that runs, and a report the CLI names
  all stay on one spelling.
- client: the config repair path tolerates the spellings a hand-edited file
  produces. `[[plugin]]` folding accepts `[[ plugin ]]` and a trailing comment
  on the header line. Duplicate top-level `plugins` lines merge onto the first
  with their ids deduplicated, and every refusal names the line it stopped on.
  `agent_install` consults the config before creating any path, so re-installing
  a registered id changes nothing on disk.
- cli: `onlyne-client init` prints the whole entry vocabulary.
  `BACKEND_COMMENTS` above `[server]` names the accepted `backend` values and
  the `[acp]` keys. `ACP_COMMENTS` below it states the closing-report contract.
  `KNOB_COMMENTS` carries the keys the code implements and the help leaves
  unprinted, with the defaults the parser applies, taken from `onlyne-config`.
  `onlyne repair` documents all seven `repair_*` verbs and the flags each one
  takes. `unknown session backend` now lists the accepted names, and the no-host
  refusal names `backend = "acp"` plus the `[acp]` keys it needs.
- config: the unread resolution layer is gone. `secret_refs`,
  `ResolvedClientConfig`, and `ResolvedEndpoint` had no caller. The client read
  the raw `ClientConfig`, and the resolved copy drifted beside it, so
  `resolve_secrets` on the config itself is now the single path. The three
  `[acp]` assertions that walked the old struct now assert the same keys through
  the resolved value.
- docs: `README.md` and `README.zh-CN.md` describe the entry as two shapes, with
  the admin nouns at the top level. They add the local `schema` and
  `completions` verbs, and name `onlyne report --help` as the authority for the
  closing-report grammar. They record the e2e directory at eighteen scripts, and
  spell the one-task grant as what it is: a role written into `allowed_targets`
  in `spec.toml`, an `onlyne reload`, and the same edit removing the edge when
  the task closes. `docs/v1-CONTRACT.md` splits the `onlyne server` line into
  the exec verbs and the in-process admin answers, and attributes the
  `missing binary …; run cargo build --workspace` refusal to the e2e harness
  where it lives. `docs/operations.md` carries the payload-v2 section and the
  `report` family, plus the paragraph stating that eight routes and an unbounded
  hop depth are the design and that `allowed_targets` is the control point.
  `crates/onlyne-client/README.md` documents `$NAME` for `cert_pin`, `key_path`,
  and `[server] host`, with the value a blank environment variable produces. The
  `handoff` module header now describes one routing path, the one the code
  takes. `onlyne-config::template` gained `NoRoleMatches`, which lists the roles
  a template directory actually holds, and `docs/v1-PLAN.md` and
  `docs/v1-ARCHITECTURE.md` print the overwrite refusal the code emits.

### Fixed

- cli: `wait-ready` reports a socket it cannot resolve the way every other verb
  does. It printed
  `onlyne: no onlyne socket found; pass --socket, --server-root, or --workspace`
  and exited 2, and the contract pins that message to exit 3. The shared helper
  already handed back 3. `admin.rs` now passes the code through, so the answer
  matches the message. A new integration case runs the verb in an empty
  directory with `ONLYNE_SOCKET` cleared and asserts exit 3, the byte-exact line
  on stderr, and empty stdout; reverting the pass-through turns that case red.
- cli: `onlyne report check` on an absent or unreadable report exits 2 and names
  the file, matching the contract's exit-code table. Code 3 stays with
  socket resolution.
- server: the gateway relay's key-set hint is one shared constant across its four
  refusal sites, so the four messages cannot drift. Each route miss now states
  the `[[route]]` row shape beside the row it could not find.
- server: `onlyne server init` writes the requeue and backend keys into the
  generated `spec.toml` with their comments. `requeue_max_attempts` and
  `requeue_ttl_secs` appear with the default that leaves the gate uncapped, and
  the block names where a role's backend and its `acp` parameters live.

All nineteen crates move to 1.2.2: `onlyne-proto`, `onlyne-frame`,
`onlyne-config`, `onlyne-layout`, `onlyne-store`, `onlyne-acp`,
`onlyne-session`, `onlyne-net`, `onlyne-adapter`, `onlyne-testkit`,
`onlyne-server`, `onlyne-client`, `onlyne-gateway`, `onlyne-gateway-telegram`,
`onlyne-gateway-feishu`, `onlyne-gateway-qqbot`, `onlyne-gateway-weixin`,
`onlyne-tui`, and `onlyne-cli`. Seven library crates carry code:
`onlyne-proto` holds the report grammar and the two shed repair fields,
`onlyne-session` journals the handoff lines ahead of the report delete,
`onlyne-client` routes them, resolves `$NAME` secrets at launch, and tolerates
the hand-edited spellings on the config repair path, `onlyne-cli` carries the
`report` family plus the `schema`, `complete`, and repair surfaces,
`onlyne-config` holds the one secret path after the dead resolution layer went
and names the roles a template directory actually holds, `onlyne-layout` owns
the per-task file names, and `onlyne-server` carries the relay's key-set hint
beside the `spec.toml` keys it now comments. `onlyne-testkit` gains e2e case 19.
The remaining eleven members — `onlyne-frame`, `onlyne-net`, `onlyne-store`,
`onlyne-acp`, `onlyne-adapter`, `onlyne-gateway`, `onlyne-tui`, and the four
gateway plugins — move with no code of their own, so every internal path
dependency keeps a matching registry floor and each manifest stays publishable on
its own.

Receipt: all nineteen crates are on crates.io at 1.2.2, published 2026-09-19,
none yanked, in the dependency order `onlyne-acp`, `onlyne-config`,
`onlyne-frame`, `onlyne-layout`, `onlyne-proto`, `onlyne-adapter`, `onlyne-cli`,
`onlyne-net`, `onlyne-session`, `onlyne-gateway-feishu`, `onlyne-gateway-qqbot`,
`onlyne-gateway-telegram`, `onlyne-gateway-weixin`, `onlyne-store`,
`onlyne-testkit`, `onlyne-client`, `onlyne-gateway`, `onlyne-server`,
`onlyne-tui`, each through `cargo publish --locked -p onlyne-<crate>` at tag
`v1.2.2` from a clean worktree. Every crate went out on its first attempt, zero
retries, and all nineteen carried the packaging sandbox build — the tree needed
neither `--allow-dirty` nor `--no-verify` this time. The sandbox builds are also
the registry-side proof: `onlyne-adapter` compiled against `onlyne-proto`,
`onlyne-frame`, and `onlyne-layout` 1.2.2 downloaded from the registry,
`onlyne-client` against `onlyne-net` and `onlyne-store` 1.2.2, and
`onlyne-gateway` against all four platform plugins 1.2.2. One cosmetic note:
cargo's availability poll for `onlyne-config` reported a timeout after its upload
landed, and the later sandbox build of `onlyne-cli` downloaded
`onlyne-config v1.2.2` from the registry. The local install was refreshed from
the same tree: `cargo build --workspace --release`, seven binaries copied to
`~/.cargo/bin` and re-signed ad-hoc, and `onlyne version` answers
`{"onlyne-cli":"1.2.2","protocol":1}` with all three daemons resolved;
`onlyne schema client` prints 149 lines.

## [1.2.1] - 2026-09-19

Scope: the ACP session gains a client-owned completion contract, and every
settled task files its receipt. Each ACP prompt ends with a directive naming
one report file under the workspace; the agent's last action is to write one
line there, and the turn's end reads that line once to decide what reaches the
ledger. Settlement itself keeps traveling the single `dispatch::on_out` path it
used before, so the change moves the source of a task's head and verdict while
the accounting surface stays. The receipt fix below completes that surface: a
task whose turn left no result line now files a `completion` row whose text is
empty, where its envelope used to fail validation and vanish.

Gate on the tree at `798d3c1`, run 2026-09-19: `cargo fmt --all --check`,
`cargo clippy --workspace --all-targets -D warnings`, and
`cargo test --workspace` give 887 passed, 0 failed, 1 ignored across 66 suites,
four above the 1.2.0 count for the three new `acp` report cases and the client
receipt case. Fake-backend e2e passes at exit 0 on `acp-session.sh` (case 18,
which now carries both report shapes), `local-task.sh`, `exec-headless.sh`,
`idempotency.sh`, `requeue-claim.sh`, `running-lights.sh`, and `acl-reject.sh`.
Against a real ACP agent, `qoderclicn --acp` served one task in a
single-role cluster in 33 seconds: the prompt reached it carrying the report
path, it wrote the file, the client consumed it, and the ledger's acked row
carried the agent's own line in `out_head` with one `completion` row beside it.

### Added

- session: the payload-v1 report contract on the `acp` backend. `deliver`
  appends a fixed directive block to every ACP prompt — its first line names
  the absolute path `<workdir>/.onlyne/out/<task-id>.md` — and the client
  creates that directory beforehand; a directory the client cannot create
  costs the directive alone, the turn runs on the task's prose, and a
  `warning` record lands beside the journal's `dispatch` record. The agent
  reports by writing exactly one line to the file, `hop-done: <the result in
  one line>` or `hop-failed: <why the task failed, one sentence>`, created
  under a temporary name in the same directory and renamed into place.
  `run_turn` reads the file once after the drain and deletes it, so a requeued
  task id starts from nothing. Absence settles as it did before the contract.
  `hop-done` replaces the head `dispatch::on_out` writes into `out_head` while
  the stop reason still decides the outcome; `hop-failed` settles the task
  Failed with the same line as its head and fault reason, downgrading a clean
  `end_turn`. A file that is empty, multi-line, bare of any prefix, prefixed
  beyond the two forms, empty after its colon, or undecodable as utf-8 settles
  the task Cancelled with head cleared and a fault reason opening with
  `acp payload invalid:` plus the category. Every read appends a `payload`
  journal record carrying `task_id`, `path`, `payload_kind` (one of `done`,
  `failed`, `invalid`, `absent`), and `head`; the absent read is recorded as
  well, so the journal names the turn that left no report. Zero config keys,
  zero CLI use inside the session, every other backend untouched. Tests: the
  thirteen-cell matrix `a_payload_report_replaces_the_head_and_can_only_lower_the_ending`,
  the shared-path test `the_prompt_hands_the_agent_the_path_the_ending_reads`,
  and the fallback test `an_unbuildable_report_directory_costs_only_the_directive`
  in `crates/onlyne-session/src/backend/acp.rs`; e2e case 18 runs both report
  shapes — a `hop-done` line that lands in `out_head` while the streamed
  answer stays out of it, and an `HOPFAIL` task settling Failed with one `acp`
  fault, its receipt filed, and its report file consumed. Files:
  `crates/onlyne-session/src/backend/acp.rs`,
  `crates/onlyne-testkit/e2e/acp-agent.py`,
  `crates/onlyne-testkit/e2e/acp-session.sh`.

### Fixed

- client: a task that ended without a result line files its receipt.
  `completion_envelope` built its body from the head alone, `new_envelope`
  runs `Envelope::validate` on the way in, and validation requires text or an
  image, so the empty-headed envelope failed and the receipt was dropped in
  silence: the task row settled `acked`, the session projected `exited` with
  its outcome, and the origin received no `completion` row for that task. A
  ring whose next hop waits on that receipt stalled exactly one hop, the shape
  observed live. The body is now `Body::text(head.unwrap_or_default())`, so a
  settled task files its receipt with the empty string as text whenever it
  carries no result line (commit `23ba012`). Files:
  `crates/onlyne-client/src/dispatch.rs`.

All nineteen crates move to 1.2.1: `onlyne-proto`, `onlyne-frame`,
`onlyne-config`, `onlyne-layout`, `onlyne-store`, `onlyne-acp`,
`onlyne-session`, `onlyne-net`, `onlyne-adapter`, `onlyne-testkit`,
`onlyne-server`, `onlyne-client`, `onlyne-gateway`, `onlyne-gateway-telegram`,
`onlyne-gateway-feishu`, `onlyne-gateway-qqbot`, `onlyne-gateway-weixin`,
`onlyne-tui`, and `onlyne-cli`. Two of them carry the behavior:
`onlyne-session` holds the report contract and `onlyne-client` holds the
receipt fix; the rest move together so every internal path dependency keeps a
matching registry floor and each manifest stays publishable on its own.
`plugins/onlyne-agent-pi` keeps its npm number `1.1.2`; the plugin's reporting
path is untouched by this round.

Receipt: all nineteen crates are on crates.io at 1.2.1, published 2026-09-19,
none yanked, in the dependency order `onlyne-proto`, `onlyne-frame`,
`onlyne-config`, `onlyne-layout`, `onlyne-acp`, `onlyne-adapter`,
`onlyne-session`, `onlyne-net`, `onlyne-gateway-feishu`, `onlyne-gateway-qqbot`,
`onlyne-gateway-telegram`, `onlyne-gateway-weixin`, `onlyne-store`,
`onlyne-testkit`, `onlyne-client`, `onlyne-gateway`, `onlyne-server`,
`onlyne-tui`, `onlyne-cli`, each through
`cargo publish --locked --allow-dirty -p onlyne-<crate>` at tag `v1.2.1`. One
upload dropped its TLS connection to crates.io mid-flight on `onlyne-adapter`,
and the retry landed that version. The fourteen head crates — `onlyne-proto`
through `onlyne-testkit` in the order above — carried the packaging sandbox
build; the five tail crates, `onlyne-client`, `onlyne-gateway`,
`onlyne-server`, `onlyne-tui`, `onlyne-cli`, went out under `--no-verify`, and
a fresh consumer project outside this workspace, `Cargo.toml` pinning `=1.2.1`
on all nineteen, resolved the whole graph from the registry and finished
`cargo check` in 43.64 seconds, which covers what the skipped sandbox builds
would have. The local install was refreshed from the same tree:
`cargo build --workspace --release`, seven binaries copied to `~/.cargo/bin` and
re-signed ad-hoc, and `onlyne version` answers
`{"onlyne-cli":"1.2.1","protocol":1}` with all three daemons resolved.

## [1.2.0] - 2026-09-19

Scope: two halves moving in opposite directions land together. The first
withdraws the session-content surface: the `onlyne-view` binary target, its
live content page over the client socket, its full-screen journal page, the
`watch_content` / `content` session subscription on the adapter protocol, and
the client-side `ContentHub`. The ACP session backend, the workspace `[acp]`
table, and the per-session journal stay, because the journal is durable record
and the backend works without a viewer pointed at it. The second half adds
three behaviors on top of the surviving surface: an ACP `initialize` that
always carries a client version, a refusal of a protocol `session_command`
handed to a pane backend, and a settlement reason readable off the ledger row.
The five rollbacks (`444caa7`, `d40d60b`, `aebb949`, `622623b`, `1228412`) and
the wording pass (`34504ed`) are new commits; the history they follow is
intact. The release scope is every crate in this workspace, nineteen at 1.2.0.
`plugins/onlyne-agent-pi` published 1.1.2 to npm earlier (`0522986`) and keeps
that number through this bump.

Gate on the tree at `8c8848d`, run 2026-09-19: `cargo fmt --all --check`,
`cargo clippy --workspace --all-targets -D warnings`, and
`cargo test --workspace` give 883 passed, 0 failed, 1 ignored across 66 suites.
Fake-backend e2e is 14/14 at exit 0 — cases 1-7, 9, 12, 14, 15, 16, 17, 18. A
five-role ring ran hops 0..10 with every hop `acked`. Two of its roles carried
`backend = "exec"` with `pi --mode rpc` and
`backend = "acp"`, and both held zero panes for the whole run: the judgement is
`cache/orca-tabs.jsonl` staying absent, or present and gaining no line. The
role carrying `backend = "orca"` with an interactive pi entered its panes as
before.

### Added

- client: `reject_protocol_command_in_pane` refuses a protocol session command
  before any pane opens. When the role's backend is `herdr`, `orca`, or
  `zellij` and the rendered `session_command` argv contains `--acp`,
  `--mode=rpc`, or `--mode rpc`, the delivery fails, the task row settles
  `rejected`, and the full sentence lands in that row's `reason` column:
  `{backend} backend cannot host a protocol session: {token} speaks JSON-RPC on
  its own stdio and the pane would print the frames; set backend = "exec" or
  backend = "acp" in the workspace config`. The guard keys on the backend name.
  A workspace chooses `backend` once per client process, so the operator's
  remedy is the config field. Files: `crates/onlyne-client/src/dispatch.rs`,
  cases in `crates/onlyne-client/tests/scenarios.rs`.
- proto, server, cli, and tui: `LedgerEntry.reason` is an `Option<String>` with
  the serde attribute
  `#[serde(default, skip_serializing_if = "Option::is_none")]`, and the read
  path carries it. `entry_from_row` copies
  `row.reason` (`crates/onlyne-server/src/relay.rs`), the CLI's row-shape probe
  `ROW_FIELD_KEYS` grows from five keys to six, with `reason` fourth behind
  `msg_id`, `task`, and `state`
  (`crates/onlyne-cli/src/ledger.rs`), and the board's second page prints
  `reason=<x>` in the task detail row's tail when the field holds a value
  (`crates/onlyne-tui/src/ui.rs`). The column and its writers predate this —
  `mark_rejected`, `fail_one`, and `expire_one` in
  `crates/onlyne-store/src/server.rs`, plus the operator `reject --reason`, which
  reaches the same `mark_rejected` — `mark_acked` takes no reason
  (`crates/onlyne-store/src/server.rs:450-452`), so an operator `ack --reason`
  travels the settlement event and leaves the row's column as it stood, and
  `repair ack` closes only the fault row. Readers could not see any of it. Values
  that have appeared
  in a live run: `requeue_exhausted`, `requeue_ttl`, `expired`, `session_dead`.
  A row an older server wrote decodes with the field empty and re-encodes
  without the key, covered by
  `a_ledger_row_without_the_reason_key_decodes_as_no_reason` in
  `crates/onlyne-proto/src/ops.rs`.

### Changed

- tui: the `onlyne-view` binary is removed with its two content views — the
  live content page fed from the client socket and the full-screen journal
  viewer. `src/view.rs`, `src/content.rs`, the `view_once` and `view_socket`
  tests, the bin section and the adapter dependency it needed are gone, and
  `render_once_text` renders the board through the single path it had before
  the shared `render_text` entry point. The board's own page set is unchanged.
  Files: `crates/onlyne-tui/Cargo.toml`, `crates/onlyne-tui/src/ui.rs`,
  `crates/onlyne-tui/src/lib.rs`.
- adapter and proto: the session-content subscription leaves the wire.
  `PluginOp::watch_content` and `HostOp::content`, `WatchContentArgs` and
  `ContentFrame` with their schema entries, the two wire-vector fixtures, the
  SDK's `Host::watch_content` seam, and the `MountKind::Admin` rule that bought
  it are gone. Files: `crates/onlyne-proto/src/adapter.rs`,
  `crates/onlyne-proto/schema/adapter.schema.json`,
  `crates/onlyne-adapter/src/lib.rs`, `crates/onlyne-adapter/PROTOCOL.md`.
- client: `ContentHub`, its subscriptions, the `watch_content` serving on the
  adapter socket, and the `run --tui` flag are removed, and with them the ACP
  backend's viewer-pane machinery: the options field, the herdr viewer backend,
  and its spawn, focus, probe, and close handling. A session is a child process
  its own client holds. Files: `crates/onlyne-client/src/content.rs`,
  `crates/onlyne-client/src/adapter_socket.rs`,
  `crates/onlyne-session/src/backend/acp.rs`.
- session: the journal and the seam that writes it stay. `ContentWriter`,
  `ContentRecord`, `read_content_records`, the offset index, and the
  `ContentSink` trait behind the backend's `set_content_sink` seam keep writing
  `<workspace>/.onlyne/logs/session-<task>.log` and
  `session-<task>.events.jsonl`. Two reporting surfaces remain: the ACP backend
  parses `outcomes()` on the client side, and pi reaches the client over the
  adapter socket plugin. The header and the `ContentSink` contract now address
  readers generally, since the surface they named is gone. Files:
  `crates/onlyne-session/src/content.rs`,
  `crates/onlyne-session/src/backend/acp.rs`.
- config: `schema/spec.schema.json` is regenerated from the current doc
  comments by `cargo run -p onlyne-config --bin config-schema`. The
  `relay_count` description gains the sentence naming the guard file's own
  spelling `relay_required_count`, and the `[server]` keys come out in the
  alphabetical order the generator emits. `config-client.schema.json` was
  already current. File: `crates/onlyne-config/schema/spec.schema.json`.

### Fixed

- acp: `ClientInfo.version` is a required `String`. ACP types `clientInfo` as
  `{name, title?, version}` with `version` a string, and a real agent answers
  `-32602 Invalid params` when the field is absent, so an `initialize` built
  from `ClientInfo::new` never reached `session/new`. `ClientInfo::new` fills
  the field from `env!("CARGO_PKG_VERSION")`, and `with_version` puts a host's
  own release number on the wire. File: `crates/onlyne-acp/src/types.rs`.

All nineteen crates move to 1.2.0: `onlyne-proto`, `onlyne-frame`,
`onlyne-config`, `onlyne-layout`, `onlyne-store`, `onlyne-acp`,
`onlyne-session`, `onlyne-net`, `onlyne-adapter`, `onlyne-testkit`,
`onlyne-server`, `onlyne-client`, `onlyne-gateway`, `onlyne-gateway-telegram`,
`onlyne-gateway-feishu`, `onlyne-gateway-qqbot`, `onlyne-gateway-weixin`,
`onlyne-tui`, and `onlyne-cli`. Every internal path dependency in
`[workspace.dependencies]`, in the crate manifests, and in
`crates/onlyne-gateway/Cargo.toml`'s four plugin entries carries the matching
registry floor, so each manifest stays publishable on its own.

Receipt: all nineteen crates are on crates.io at 1.2.0, published 2026-09-19,
none yanked, in the dependency order `onlyne-proto`, `onlyne-frame`,
`onlyne-config`, `onlyne-layout`, `onlyne-acp`, `onlyne-adapter`,
`onlyne-session`, `onlyne-net`, `onlyne-gateway-feishu`, `onlyne-gateway-qqbot`,
`onlyne-gateway-telegram`, `onlyne-gateway-weixin`, `onlyne-store`,
`onlyne-testkit`, `onlyne-client`, `onlyne-gateway`, `onlyne-server`,
`onlyne-tui`, `onlyne-cli`, each through
`cargo publish --locked --allow-dirty -p onlyne-<crate>` at tag `v1.2.0`. A
fresh consumer project outside this workspace, `Cargo.toml` pinning `=1.2.0` on
`onlyne-cli`, `onlyne-server`, `onlyne-client`, `onlyne-gateway`, `onlyne-tui`,
`onlyne-acp`, `onlyne-testkit`, and `onlyne-gateway-telegram`, resolved the
whole graph from the registry and finished `cargo check` in 35.7 seconds.
`onlyne-cli` answers the consumer with `ignoring invalid dependency ... missing
a lib target`: the crate ships the `onlyne` binary and no library, which is the
shape `cargo install` reads. The local install was refreshed from the same
tree: `cargo build --workspace --release`, seven binaries copied to
`~/.cargo/bin` and re-signed ad-hoc, and `onlyne version` answers
`{"onlyne-cli":"1.2.0","protocol":1}` with all three daemons resolved.

## [1.1.1] - 2026-09-18

Scope: the socket-path field fix reported 2026-09-17 from the formal-research
tree, plus the herdr argv fixes from the same session. On macOS `sun_path`
holds 104 bytes including its NUL, and a generated role workspace nests three
levels below its server root
(`<root>/.onlyne/ws/<topology>/<role>/.onlyne/run/s`), so seven of the eleven
clients in that swarm could not bind the adapter socket. Each logged
`adapter socket restarting error=bind <path>` on a half-second loop, the pi
plugin logged `connect EINVAL <path>`, and `onlyne status` kept reading
`connected_roles=11`: the TLS half of every role was healthy, the local half
was dead, and nothing said so. The same run reported the fifth field defect — a
completed session kept its host pane alive — and its companion: a task already
acked as complete was reported `stalled` half an hour later.

### Added

- layout: `SocketEndpoint` with `socket_path()`/`bind_socket` on both owner
  trees. On unix a daemon binds the canonical `<root>/.onlyne/run/s` while that
  path fits `UNIX_SOCKET_PATH_MAX` = 103 bytes, and past the bound a short
  derived path `<temp_dir>/onlyne-<16hex>/s` (the hex is a sha256 prefix over
  the canonical owner root). The bound path is published in
  `<root>/.onlyne/run/socket` (mode `0600`, one path plus a newline), and every
  finder — the `onlyne` CLI, `onlyne-tui`, the fake agent, the pi plugin —
  reaches the served path through the owner tree. Files:
  `crates/onlyne-layout/src/lib.rs`, `crates/onlyne-layout/src/local_socket.rs`.
- client and cli: the client injects the served adapter-socket path into every
  session it spawns as `ONLYNE_SOCKET`, and the `onlyne` CLI honors
  `ONLYNE_SOCKET` — after `--socket`, before `--server-root`/`--workspace` — so
  a shell inside a role pane reaches the socket without spelling it. Files:
  `crates/onlyne-client/src/dispatch.rs`, `crates/onlyne-cli/src/flags.rs`,
  `crates/onlyne-cli/src/socket.rs`.
- e2e: verification case 17 `socket-path-length.sh` builds a workspace whose
  canonical socket path is past 103 bytes with padding, runs the client and the
  fake agent there, asserts the served path is short, published in
  `run/socket`, holding a bound socket, with the canonical path left bare and
  the client log naming the served path; then `onlyne --workspace <deep ws>
  who` answers `planner` and one task settles `acked` with the session
  `exited`/`done`. File: `crates/onlyne-testkit/e2e/socket-path-length.sh`.

### Changed

- session: `herdr agent start <name> --kind <k> --pane <id> --timeout 25000`
  carries the `session_command` tail after `--`
  (`-- --session-id <id> --session-dir .pi/sessions`), the call shape herdr
  0.9.0 documents; the same argv without the separator answers `unknown
  option: --session-id` with exit 2. `--cwd` travels absolute in `workspace
  create`, `tab create`, and `pane split`, since herdr resolves a relative
  `--cwd` against its own working directory. The create path for a herdr
  workspace now warns with the label, the new `workspace_id`, and the remedy
  `herdr workspace rename <WORKSPACE_ID> onlyne:<cluster>`. File:
  `crates/onlyne-session/src/backend/herdr.rs`.
- client: `onlyne-client run --workspace <rel>` resolves the workspace to an
  absolute path before use. File: `crates/onlyne-client/src/main.rs`.
- cli and tui: socket discovery resolves through the owner tree — a directory
  owns a surface when `.onlyne/run/s` or `.onlyne/run/socket` answers — so the
  walk reaches a deep workspace's short served path. Files:
  `crates/onlyne-cli/src/socket.rs`, `crates/onlyne-tui/src/socket.rs`,
  `crates/onlyne-testkit/src/lib.rs`.
- e2e: live verification case 13 `herdr-live.sh` now closes the resource loop.
  The case reads the session pane's shell pid through `herdr pane
  process-info`, drives `onlyne control ... recycle` on that session, then
  asserts the pane id is gone from `herdr pane list` and `kill -0` finds the
  recorded pid gone, with the tab's `pane_count` back to 1 and the surviving
  pane still the recorded root. The SIGTERM drain follows the closed session, and
  the case keeps its `SKIP herdr-live` discipline plus its cleanup of every
  workspace it created. File: `crates/onlyne-testkit/e2e/herdr-live.sh`.

### Fixed

- client: a session's host resource retires with the session. The observed shape
  was a planner tab holding three panes — two empty shells left behind by
  completed sessions plus the live one — and two older residuals still running a
  full `pi` process tree (`volta-shim` → `node` → `bun`) after their sessions
  read `lifecycle=exited`; each needed a manual `herdr pane close`. The rule
  now: a pane, tab, zellij session, or exec child closes when its session holds
  no task and no plugin transport is attached. Three triggers carry that rule — a
  graceful plugin `detach` closes the resources of the idle sessions that
  connection served, a settle with no attached agent closes at settle time, and
  the 250 ms readiness tick closes any idle tracked session that carries a stored
  outcome while its stored lifecycle projects `Exited` and its agent is gone,
  taking the reason from that outcome (`done` → `Completed`, `failed` → `Fault`,
  `cancelled` → `Cancelled`).
  A connection that ends without a `detach` keeps the resource for an agent that
  may reconnect, and a settled session whose agent is still attached keeps it for
  the next task `reuse` hands over. While the stored resource state is still
  open, each retirement refreshes a stale reference through `backend.attach`,
  projects `resource_closed`, and logs `retiring idle session resource` with the
  task, backend, resource, and reason; a close failure lands as a warning. Files:
  `crates/onlyne-client/src/dispatch.rs`, `crates/onlyne-client/src/runloop.rs`,
  `crates/onlyne-client/src/adapter_socket.rs`.
- client: a completed task stays out of the stall watch. The shape on the wire
  was a `stalled` fault with reason `no applied progress` landing 30 minutes (the
  `stall_report_secs` default of 1800) after an acked completion — faults id 5 on
  `da069be7` and id 7 on `53da164d`. When its previous beat read something other
  than `idle`, the pi plugin answers one observation after the completion reply —
  a heartbeat carrying `agent: "idle"` — and that heartbeat path called
  `StallWatch::note_applied`, which opened a fresh progress clock for a task the
  settle path had just forgotten. `note_applied` now refreshes an assigned clock,
  a connection release forgets the clocks of the sessions it served, the due scan
  suppresses and forgets any task whose stored lifecycle projects `Exited`, and
  `stall_report` repeats that lifecycle check at the send boundary. Clearing the
  non-working projection rows out of `state.db` left the fault intact, since the
  clock lives in client memory, and a restart of that role's client stopped it.
  Files: `crates/onlyne-client/src/stall.rs`,
  `crates/onlyne-client/src/dispatch.rs`.
- session: `herdr pane close` answering `pane_not_found` is a success. The close
  logs `herdr pane already closed` at debug, so a workspace that disappeared on
  its own stops reading as `session close failed during shutdown`. File:
  `crates/onlyne-session/src/backend/herdr.rs`.
- client: `onlyne-client run` exits 1 with `onlyne-client: bind the workspace
  socket <canonical path>: <detail>` when the adapter socket cannot be bound —
  the detail names the served path, both byte lengths, and the OS reason —
  because a client whose local surface is dead still holds its TLS link and
  still reads as connected. An `accept` error after a successful bind logs at
  `error` level (`adapter socket accept failed; retrying`) and retries on a
  100 ms interval with the listener held. The silent half-second rebind loop is
  gone. Files: `crates/onlyne-client/src/runloop.rs`,
  `crates/onlyne-client/src/adapter_socket.rs`,
  `crates/onlyne-client/tests/scenarios.rs`.
- server: `onlyne-server` logs the served path at startup — the short-path case
  names both spellings with the canonical length — so the marker's answer is
  visible in the log from the first line. File:
  `crates/onlyne-server/src/admin.rs`.

All eighteen crates move to 1.1.1: `onlyne-proto`, `onlyne-frame`,
`onlyne-config`, `onlyne-layout`, `onlyne-store`, `onlyne-session`,
`onlyne-net`, `onlyne-adapter`, `onlyne-testkit`, `onlyne-server`,
`onlyne-client`, `onlyne-gateway`, `onlyne-gateway-telegram`,
`onlyne-gateway-feishu`, `onlyne-gateway-qqbot`, `onlyne-gateway-weixin`,
`onlyne-tui`, and `onlyne-cli`. Tag `v1.1.1`. Install:
`cargo install onlyne-cli --version 1.1.1` plus
`onlyne-server onlyne-client onlyne-gateway onlyne-tui` at the same version.

## [1.1.0] - 2026-09-15

Scope: the exec session backend as a first-class headless path, a Windows
named-pipe local-socket seam, zellij probe mapping, and dual-job CI. All
eighteen crates move to 1.1.0.

`cargo test --workspace` on 2026-09-15: 760 passed, 0 failed, 1 ignored
(`herdr_live_probe`). Fake-backend e2e is 12/12, including
`crates/onlyne-testkit/e2e/exec-headless.sh`. fmt and clippy are clean.

### Added

- session: workspace `config.toml` carries optional `backend`.
  Selection is env `ONLYNE_BACKEND` (nonempty) > that field > auto. Parse
  accepts `headless` as an alias for `exec`; `BackendName::as_str` and every
  projection still write `exec`. On process exit, `probe` copies the session
  log tail into `ResourceProbe.detail.output_tail` (at most 200 lines, 16 KiB
  byte window first). Windows spawn sets `CREATE_NEW_PROCESS_GROUP`; close
  sends `CTRL_BREAK`, waits the grace window, then `child.kill()`. A client
  with no console skips the console event and terminates the child. Files:
  `crates/onlyne-config/src/client.rs`, `crates/onlyne-session/src/backend/exec.rs`,
  `crates/onlyne-session/src/backend/mod.rs`, `crates/onlyne-client/src/runloop.rs`.
- layout: `interprocess` 2.4.4 (`tokio`) is the local-socket seam. Unix keeps
  a filesystem UDS at `.onlyne/run/s` (mode `0o600`). Windows stable 1.85 has
  no tokio `UnixStream` (`cfg(unix)`), so `.onlyne/run/s` is a marker file
  `v1:onlyne-<32hex>` and the NPFS leaf is `sha256` of the lexical-absolute
  path (separators `/`, lowercased) truncated to 16 bytes hex. Bind uses
  owner-only SDDL `D:P(A;;GA;;;OW)(A;;GA;;;SY)`. `--socket` values that start
  with `\\.\pipe\` travel verbatim. `ERROR_PIPE_BUSY` maps to `WouldBlock` and
  retries inside CLI `--timeout`. Exit codes 2/3/4/5 stay. Files:
  `crates/onlyne-layout/src/local_socket.rs`, `crates/onlyne-cli/src/wire.rs`.
- ci: `.github/workflows/ci.yml` runs two jobs. `linux` on `ubuntu-latest`
  does `cargo fmt --all --check`, `clippy --workspace --all-targets -D warnings`,
  and `cargo test --workspace`. `windows` on `windows-latest` tests the core
  crate subset (`onlyne-proto` through `onlyne-tui`).
- e2e: verification case 16 `exec-headless.sh` assigns through a workspace
  `backend = "headless"` field, runs the fake agent as `session_command`,
  asserts the session log, an `exec` backend string in `client.db`, and
  `exited`/`done`. File: `crates/onlyne-testkit/e2e/exec-headless.sh`.
- dependencies: workspace `interprocess` 2.4.4. Windows exec and server
  process helpers declare `windows-sys` 0.61 (Console/Process/Threading).
  `interprocess` 2.4.4 itself depends on `windows-sys` 0.61.2 on that target.

### Changed

- session: zellij `probe` lists sessions without `--short`, then on a live
  session runs `action list-panes --json --state`. An `EXITED` listing reports
  `alive: false` with `reason: session_exited`. A pane with `exited` or
  `is_held` reports `alive: false` and the host `exit_status` when present.
  herdr and orca probes stay: those hosts expose no integer exit code on the
  pane/tab the client already queries. File:
  `crates/onlyne-session/src/backend/zellij.rs`.

### Fixed

- session: zellij `probe` reads `list-sessions` with the EXITED marker intact.
  `--short` stripped that marker and an EXITED session listed as a name was
  reported alive.

All eighteen crates are on crates.io at 1.1.0, published 2026-09-15, none
yanked: `onlyne-proto`, `onlyne-frame`, `onlyne-config`, `onlyne-layout`,
`onlyne-store`, `onlyne-session`, `onlyne-net`, `onlyne-adapter`,
`onlyne-testkit`, `onlyne-server`, `onlyne-client`, `onlyne-gateway`,
`onlyne-gateway-telegram`, `onlyne-gateway-feishu`, `onlyne-gateway-qqbot`,
`onlyne-gateway-weixin`, `onlyne-tui`, and `onlyne-cli`. Tag `v1.1.0` is
`e2d0e15`. CI run 34977562567 is green on linux and windows. A consumer
project resolved and compiled all five shipped binaries from the registry
(`cargo add onlyne-cli onlyne-server onlyne-client onlyne-gateway
onlyne-tui`, each pinned 1.1.0; `cargo check` finished with zero errors).
macOS release binaries are ad-hoc codesigned into `~/.cargo/bin`;
`onlyne --version` prints `1.1.0`.

## [1.0.9] - 2026-09-15

Scope: delivery truth. The workspace version moves 1.0.3 → 1.0.4, so
`onlyne-proto`, `onlyne-config`, and `onlyne-tui` ride it to 1.0.4. The pinned
crates: `onlyne-client` 1.0.6 → 1.0.7, `onlyne-store` 1.0.4 → 1.0.5, and
`onlyne-server` 1.0.8 → 1.0.9.

The trigger was the ARIS takeover frame, read back through the full event log
after the swarm stopped. One mechanism explained every symptom: a role link
death requeues that role's `in_flight` rows at the next hello regardless of
whether the client still runs the task, the re-delivery flip through `pull`
wrote the ledger without publishing a `ledger_state` event, and the offline
reader saw rows `queued` while the session axis said `working` for six hours.
A client restart erases the in-memory dedup the design leaned on, so the
scheduled morning boot would have opened a second session on each surviving
pane. The fix moves the liveness fact onto the handshake, makes every delivery
flip loud, and gives the automatic requeue a budget the operator can bound.

### Fixed

- protocol and server: `HandshakeArgs.live_tasks` carries the task ids whose
  slots the client still holds in memory. Adoption requeue skips claimed rows
  and rehangs their delivery tickets on the new link generation, so the next
  teardown reclaims them normally and the running session is never handed a
  second copy of its own task. An absent or empty list keeps the 1.0.8 behavior
  for older clients. The claim reads the dispatch slots, not the store: a fresh
  client process declares nothing, which is what keeps the restart-and-pull
  recovery of verification case 4 intact. Files:
  `crates/onlyne-proto/src/ops.rs`, `crates/onlyne-server/src/relay.rs`,
  `crates/onlyne-server/src/router.rs`, `crates/onlyne-server/src/state.rs`,
  `crates/onlyne-client/src/claim.rs`, `crates/onlyne-client/src/dispatch.rs`.
- server: a claimed session that dies without completing releases its row.
  The `session_sync` path calls `relay::release_exited_delivery` when an applied
  write lands `Exited`: a matching `in_flight` row whose ticket names the same
  `session_id` goes back through the automatic requeue and its ticket drops, so
  the next pull re-delivers. A completion that already acked the row leaves
  nothing for the hook to move. Files:
  `crates/onlyne-server/src/projection.rs`, `crates/onlyne-server/src/relay.rs`.
- server: the `pull` path published nothing when it flipped a queued row to
  `in_flight`, which is the silent half of the contradiction above. The flip now
  emits the same nine-observable `ledger_state` event the push path emits.
  File: `crates/onlyne-server/src/relay.rs`.

### Added

- server and store: the automatic requeue honors two spec gates, evaluated at
  the single funnel `relay::requeue_role_rows` and the exited-release hook: TTL
  first (`requeue_ttl_secs`, expired row with reason `requeue_ttl`), then budget
  (`requeue_max_attempts`, rejected row with reason `requeue_exhausted`). The
  `ledger` gains a `requeued` column through the in-place `ensure_` ALTER,
  incremented inside `requeue_one`; the schema marker stays `('onlyne-server',
  1, 1)`. Both defaults keep today's unlimited loop; `repair retry` and the rest
  of the repair family ride outside the gates by design. Files:
  `crates/onlyne-config/src/spec.rs`, `crates/onlyne-store/src/server.rs`,
  `crates/onlyne-server/src/relay.rs`.
- client: `stall_report_secs` (config.toml, default 1800, 0 disables) reports
  the progress freeze the heartbeat cannot see. The stall clock starts at
  assignment and refreshes only on `Applied` persists; a no-op beat keeps the
  row alive without counting as progress. A running session frozen past the
  threshold sends one `Report::Fault{kind:"stalled"}` per episode over the
  existing report path, re-armed by the next `Applied` change. The row keeps its
  state; the fault table gets the observation. Files:
  `crates/onlyne-client/src/stall.rs`, `crates/onlyne-client/src/dispatch.rs`,
  `crates/onlyne-client/src/runloop.rs`, `crates/onlyne-config/src/client.rs`.

### Changed

- protocol: the `GatewayOp` size ceiling moves 128 → 256. The handshake Vec
  grows the hello arm on both role and gateway vocabularies, and the ceiling
  test records the raise deliberately. File:
  `crates/onlyne-proto/tests/sizes.rs`.
- dependency floors published with this release: `onlyne-server` requires
  proto ≥ 1.0.4, config ≥ 1.0.4, store ≥ 1.0.5; `onlyne-client` requires
  proto ≥ 1.0.4 and config ≥ 1.0.4; `onlyne-tui` requires proto ≥ 1.0.4.
- e2e: verification case 15 `requeue-claim.sh` kills the server under a live
  link, restarts it, and asserts the claimed row rides the restart with one
  delivery event, zero requeues, one working session, and a natural ack.
  Files: `crates/onlyne-testkit/e2e/requeue-claim.sh`.

All six are on crates.io: `onlyne-proto` 1.0.4 at 2026-09-15T03:43:55Z,
`onlyne-config` 1.0.4 at 03:45:08Z, `onlyne-store` 1.0.5 at 03:45:24Z,
`onlyne-client` 1.0.7 at 03:45:50Z, `onlyne-tui` 1.0.4 at 03:46:13Z, and
`onlyne-server` 1.0.9 at 03:46:29Z, none yanked. The publishes ran as
`cargo publish --allow-dirty --locked -p onlyne-<crate>` in that dependency
order through the network window that opened at 11:43 local after the 1.0.8
binaries had already been copied to `~/.cargo/bin`. A fresh consumer project
outside this workspace resolved the raised floors and compiled
`onlyne-server` 1.0.9 with `onlyne-client` 1.0.7 and `onlyne-tui` 1.0.4 in 23
seconds, so `cargo install` now carries the claim handshake to every host,
the ARIS swarm included.

## [1.0.8] - 2026-09-15

Scope: heartbeat liveness from the client's beats to the server's own sweep. The
workspace version moves 1.0.2 → 1.0.3, so `onlyne-proto`, `onlyne-config`, and
`onlyne-tui` ride it to 1.0.3 with their changed code and the remaining inherited
crates move beside them. The pinned crates: `onlyne-client` 1.0.5 → 1.0.6,
`onlyne-store` 1.0.3 → 1.0.4, and `onlyne-server` 1.0.7 → 1.0.8.

The trigger came from a live swarm: an Orca pane whose process vanished left its
session row `working` and `attached` forever while the role link stayed online.
The row's age was invisible on the server, the stale watch skips online roles,
and the lifecycle owner had nothing left to publish. The fix puts liveness on
the wire the server already controls: the `pi-onlyne` heartbeat is the liveness
fact, every beat re-publishes the session row, and the server times beats the
same way it already times role presence.

### Fixed

- client and store: a heartbeat whose observed state matched the stored one
  answered `Ignored(NoOp)`, the dispatch arm read that as "nothing changed", and
  no `session_sync` followed. A healthy session that thinks quietly keeps the
  same tuple, so its server row froze at the last state change: `onlyne sessions`
  `updated_at` aged for hours while the agent beat every ten seconds. Every beat
  now republishes. An applied verdict syncs as before. A no-op beat runs through
  the new `ClientLedger::bump_session_version`, which moves the local
  generation/seq under the same strictly-greater gate the server projection
  uses, and the arm syncs when the bump lands. Rejected and stale-sequence beats
  stay quiet, so a delayed duplicate cannot push the server's clock. Files:
  `crates/onlyne-client/src/dispatch.rs`, `crates/onlyne-store/src/client.rs`.
  Tests: `crates/onlyne-store/src/tests.rs` (bump gate), `crates/onlyne-client`
  scenarios (two no-op beats produce two `session_sync` frames — red with zero
  frames before the change, plus the sync content check).

### Added

- server, proto, config, and tui: the stale watch times heartbeats for
  online-role rows. `[server].heartbeat_grace_secs` (default 90) is the silence
  budget for a `working` row whose role link is up; past it the scan records a
  `heartbeat_missing` fault once per task while the fault stays open, and the
  row itself stays `working` — the flag belongs to the supervisor's desk. Rows
  the sweep may flag are rows this process has seen a session write for, and
  `Server::open` inherits the previous process's `working` rows into that set,
  so a restart observes a stuck row within one grace window instead of waiting
  for writes that a dead session never sends. A heartbeat that lands on an
  `exited` row of the same generation lifts the row back to `working` and
  records `heartbeat_after_complete` beside it. `SessionRow` answers carry
  `heartbeat_stale` — absent while fresh, pinned by the sizes suite — across
  `query_sessions`, `AdminOp::Sessions`, and the TUI, which renders the state as
  `working+stale`. Files: `crates/onlyne-server/src/stale.rs`, `state.rs`,
  `projection.rs`, `crates/onlyne-proto/src/ops.rs`, `crates/onlyne-config/src/spec.rs`,
  `crates/onlyne-tui/src/ui.rs`. Tests: five stale units (grace, dedup, seen
  gate, revival), six delivery tests including
  `a_row_working_at_open_is_watchable_without_a_new_write` (a real reopen: the
  inherited row answers `heartbeat_missing` with no new write) and
  `a_stale_working_row_answers_heartbeat_stale_true_on_both_surfaces`, the
  `heartbeat_after_complete` unit, the sizes pin, the config contract keys, and
  the TUI mark. Verification case 14 `heartbeat-watch.sh` runs the whole chain
  through real binaries: one scripted beat keeps the row fresh, the quiet that
  follows produces the fault and the flag inside the grace window, and the row
  never flips. A release-binary run replayed the incident: a server holding a
  `working` row took `kill -9`, the reopened 1.0.8 process watched the role
  reconnect, and the inherited row answered `heartbeat_missing` and
  `heartbeat_stale` six seconds in while the row stayed `working`.

## [1.0.7] - 2026-09-14

Scope: the queued note deadline and the role link's write path. `onlyne-server`
1.0.6 → 1.0.7, `onlyne-store` 1.0.2 → 1.0.3. Every other crate stays where it is.

Both are on crates.io: `onlyne-store` 1.0.3 at 2026-09-14T01:35:17Z and
`onlyne-server` 1.0.7 at 01:35:34Z, neither yanked. `publish` is a per-crate switch,
so the workspace-level `publish = false` stays inert, and the two publishes ran as
`cargo publish --allow-dirty --locked -p onlyne-store` then the same for
`-p onlyne-server`. A fresh consumer project outside this workspace resolved
`onlyne-server` 1.0.7 with `onlyne-store` 1.0.3 and `onlyne-proto` 1.0.2 from the
registry and compiled them.

### Fixed

- server and store: a queued note's deadline lived in the process's memory, and
  a restarted server left the row `queued` forever. The only arming site was
  `relay::send`, and `sweep_expired` reads that map, so a deadline had no path back
  into a fresh process. The deadline is now a ledger column (`expires_at`) armed again
  when `State` opens, written for a `note` row whose envelope carries `ttl_ms` — the
  pair `onlyne send --ttl --note` produces. The marker version stays 2 and the column
  is added in place to existing databases, so a live workspace keeps its rows. Files:
  `crates/onlyne-store/src/server.rs`, `crates/onlyne-server/src/state.rs`,
  `crates/onlyne-server/src/relay.rs`. Tests: `crates/onlyne-store/src/tests.rs`
  (persisted deadline, `pending_expiries` order, in-place column add) and
  `crates/onlyne-server/tests/delivery.rs`
  (`a_restarted_server_rearms_queued_note_deadlines`, which fails with an empty sweep
  when the re-arm is lifted). A row queued by an older binary keeps `expires_at` null:
  that binary wrote the deadline nowhere, so the sweep has nothing to arm and the row
  stays `queued` for an operator's `repair_fail` or `repair_close` to settle. Both
  halves were run through real processes against a database written by the installed
  1.0.6 server: the pre-upgrade row stayed `queued`, and a note sent after the upgrade
  stored its deadline and settled `expired` in a later server process.
- server: three write sites in `role_connection` answered a failed frame write with
  `?`, which returned before the generation check and `relay::disconnect`. A role
  whose link died mid-write stayed registered, online to the sender gate, holding
  its rows `in_flight` with a ticket naming a dead connection until that role's next
  `hello`. The write failure now ends the loop through the same door as EOF, so the
  requeue, the registry removal, and the `offline` presence happen on the spot. The
  `RoleIo` seam in `crates/onlyne-server/src/lib.rs` exists so a write failure
  reaches that teardown, and a duplex stream reaches it in a test while a TLS socket
  cannot be aimed at one. Test:
  `a_write_failure_requeues_in_flight_rows_and_marks_the_role_offline`.

## [1.0.6] - 2026-09-14

Scope: the herdr backend, the focus chain, and the control plane's delivery path.
Versions published by this release: `onlyne-proto` 1.0.1 → 1.0.2 and `onlyne-tui`
1.0.1 → 1.0.2 ride the workspace version, `onlyne-session` 1.0.2 → 1.0.3,
`onlyne-server` 1.0.5 → 1.0.6, `onlyne-client` 1.0.4 → 1.0.5, and `onlyne-cli`
1.0.1 → 1.0.2, the version the last round set and this one publishes.
`onlyne-adapter` and `onlyne-testkit` changed code and ride the workspace version
to 1.0.2 beside them. The remaining crates reach 1.0.2 with their code unchanged.

### Added

- session: a herdr backend at `onlyne-session/src/backend/herdr.rs`, selected by
  `ONLYNE_BACKEND=herdr`. Hierarchy: the herdr session is inherited from the
  client process environment, one workspace labelled `onlyne:<cluster>` per
  server root, one tab per role, one pane per onlyne session. `herdr pane close`
  terminates the pane's process tree, so a session never outlives its pane.
  Spawn runs two tracks: a `session_command` whose first token names a known
  agent (`pi`, `omp`, the rest of herdr's `--kind` table) goes through
  `herdr agent start --kind`, and a command herdr's table does not name goes
  through `herdr pane run` as one `shell_quote`d line. Split placement is
  `PanePlacement::from_pane_count`: `(count + 1).is_power_of_two()` maps to
  `right`, remaining counts map to `down`, ratio `0.5`, where `count` is
  `result.tabs[].pane_count` from `herdr tab list --workspace W` and a missing
  field is `0`. Focus walks `herdr workspace focus <W>` → `herdr tab focus <T>`
  → `herdr agent focus <pane_id>` for a managed pane, or
  `herdr pane focus --pane <base_pane> --direction <split_direction>` for a shell
  pane, and confirms with `herdr pane get <pane_id>`. Every host call is a JSON
  argv call: the shell layer is gone, so no argument is ever re-parsed.
- session: host detection at `onlyne-client/src/host.rs`. `ONLYNE_BACKEND` names
  `herdr | orca | zellij | exec | fake | auto`; an empty value or `auto` probes
  herdr, then orca, then zellij; `exec` and `fake` require their own name; no
  match returns `NO_SUPPORTED_HOST` naming the three probed hosts.
- client: `onlyne-client doctor`, a read-only verb printing one JSON object
  (`host`, `backend_selection`, `explicit`, `binary`, `session`, `workspace_id`,
  `tab_id`, `pane_id`, `refusal`) with exit 0 — a pre-deploy check for a host
  that may not answer.
- testkit: e2e case 13, `crates/onlyne-testkit/e2e/herdr-live.sh`: live herdr
  session, `session_command = ["sleep", "600"]` on the `pane_run` track,
  `max_sessions` 1. It asserts the workspace label derived from the generated
  spec, the role tab, `pane_count` reaching 2 with both pane ids, the address in
  `client.db` `sessions.backend_ref`, `onlyne control --from planner focus
  --task` landing on the session pane (`pane get` reports `focused`), the drain
  path keeping the tab's root pane, and a workspace list back to its pre-run
  snapshot. `lib.sh` gains `stop_client_sync`, which waits for the client's
  `run/s` socket to disappear so the next client can hold the role link. A
  missing herdr binary or an unreachable `HERDR_SESSION` prints `SKIP herdr-live`
  and exits 0.

### Changed

- client: `onlyne-client run` exits 5 when host detection finds no supported
  host.
- proto: `PullArgs` carries optional `control_only`, default false. Old frames
  decode, and the JSON Schema under `crates/onlyne-proto/schema/` is regenerated.
- server: `pull` honours `control_only` by handing rows whose kind is `control`
  and leaving `task`, `relay`, and `notice` rows `queued` with their ticket
  untouched, on the re-offer path too.
- client: a role at `max_sessions` pulls with `control_only`. That is the path
  `focus`, `recycle`, and `cancel` take to reach the session holding the last
  free slot.
- client: the herdr workspace label comes from `welcome.cluster`, the server's
  own `[server] name`, passed to every pane the client creates as
  `ONLYNE_CLUSTER`. A pane created before the first welcome uses herdr's default
  workspace.
- tui: `F` sends a focus control op for the selected session through the admin
  socket (`control --from <role> focus --task <id>`), taking the sender from the
  selected row's causality, and prints what the daemon answers. Key handling moved
  into a table in `model.rs` (`interpret_key`), so every binding including `F` is
  a pure function with table tests.

### Fixed

- client and server: a role at capacity never received control. `pull_ack_loop`
  stopped pulling when its dispatch table filled, and control rows share that
  queue, so `control focus`, `recycle`, and `cancel` sat on the server forever
  for the session holding the last free slot. Field evidence: a `focus` row
  `in_flight` at attempt 0 with no fault and nothing in the client log.
- client: every onlyne-owned herdr workspace landed on `onlyne:default` because
  nothing injected `ONLYNE_CLUSTER` into a spawned pane, so two server roots
  shared one workspace. The label now carries the server's own name, and the
  e2e case reads it from the spec it generated.
- server: `control focus` answered `unknown_role` for a role the requester holds
  a `send` edge to. The admin ACL now reads control the way `router::accept`
  reads it at delivery: a role with a `send` edge to the target owns control of
  that target's sessions, and `ControlOp::Broadcast` requires a global edge.
- session: `close` refuses an id that a `backend_ref` was re-labelled with, which
  keeps `onlyne:*` workspaces, role tabs, and foreign panes out of reach of a
  forged reference; `focus` and `close` check the daemon answer and report a
  refusal.
- client: an always-running agent now carries every task of the session it takes.
  A plugin that mounts naming no session is parked as the connection for the next
  staged session, and the claim took that socket with nothing recording which
  session it served. The second task a `reuse` role gives that session was left
  with a payload and no transport: the assignment never left the client, the
  plugin waited on `assign`, and the ledger held the envelope `in_flight` at
  attempt 0. The claim binds the connection it takes to the session it hands
  over; a mount arriving after a task hands that session over on the spot; and a
  connection that named no session releases only the transports sharing its
  socket (`same_connection` on `AdapterIo`). `crates/onlyne-client/src/dispatch.rs:355`,
  `crates/onlyne-client/src/adapter_socket.rs:336`. Regression tests
  `a_parked_agent_carries_every_later_task_of_its_reused_session` and
  `a_session_staged_before_its_agent_mounts_is_handed_its_payload` in
  `crates/onlyne-client/tests/scenarios.rs` both time out on the missing
  assignment with the fix reverted. Field cost: `running-lights` hopped six of
  eight lights on the released tree and stalled at hop 0 on the commit before —
  one defect, whichever session the park happens to serve first.
- server: a note aimed at a role with nothing to wake took a delivery the
  recipient could only refuse. §7 gives a note no session of its own, and `pull`
  hands out no row for one, so a role whose sessions have all settled has nowhere
  to put it. The gate now reads that state beside presence: an online role with no
  `working` session is refused before the ledger row exists, with a message naming
  the missing session, and a row that `note_queue` does queue stays `queued` for
  its `ttl_ms` deadline, which is the only exit a note has. Field evidence:
  verification case 6's ttl note settled `rejected` with
  `note has no live session to wake`, the receiving half of 1.0.5 answering a push
  the sender could have been given straight away. `onlyne-server` 1.0.6,
  `crates/onlyne-server/src/state.rs:273`, `crates/onlyne-server/src/relay.rs:382`.
  Tests `a_note_needs_a_live_session_on_the_receiving_role` and
  `a_queued_note_for_a_role_with_nothing_to_wake_expires_on_its_ttl`; case 6 now
  proves the refusal against an idle online role and the queued path against the
  offline one under `note_queue = true`.
- testkit: `running-lights` sighted the light through a render choice. The
  scripted frame is one `onlyne-tui --once` picture, and page 1 lays its nodes out
  with a force simulation whose detail level follows the camera — at the fixed
  `--once` zoom a node box carries a title alone, so most roles never drew the
  `◐` the case looked for and a completed twelve-hop ring still failed its
  sighting. The case reads page 2 now, where the graph is a table of
  `role · task · life · agent · in-flight`, and asks for that role's `working` row
  beside the same hop in the ledger. `crates/onlyne-testkit/e2e/running-lights.sh:145`.

## [1.0.5] - 2026-09-13

Scope: the role link. `onlyne-server` 1.0.4 → 1.0.5, `onlyne-client` 1.0.3 →
1.0.4, published to crates.io. `onlyne-session` stays at 1.0.2, `onlyne-cli` at
1.0.2, every other crate unchanged.

### Fixed

- server: a row pushed into the gap between a role's death and its next `hello`
  stayed in flight with no puller able to take it. A push marks its row
  `in_flight` before the frame reaches a socket, the departed link's teardown
  skips its requeue once the registry generation has moved past it, and a
  role-level pull passes by a row whose ticket is still armed. The registry
  keeps one link per role, so the `hello` that registers a link now requeues
  that role's in-flight rows, which is the move a clean `bye` already makes:
  row back to `queued`, ticket dropped, one `ledger_state` event per row. Field
  report: a close-out ticket pushed at 13:48 landed in the window where the old
  client was dead and the replacement had not linked, and an operator SIGTERM
  of the new client was what returned the row to `queued`. A link flap now
  re-delivers a role's unacknowledged rows at hello, where the client's `op_id`
  dedup and its session-keyed slot keep that from running the work twice.
- client: a restarted role took 300 seconds to start pulling. The residual
  sweep waits out `stale_grace_secs` (300 by default) for a plugin mount when
  the role holds residual acked working rows, and it ran inline ahead of the
  pull, flush, event, and readiness loops, so a role came up with an
  authenticated link and no delivery loop for five minutes (measured: spawn
  13:46:51, `server link ready` 13:51:51). The sweep now runs detached once
  those four loops are live, and it logs its own failure, leaving the link
  serving.

## [1.0.4] - 2026-09-13

Scope: the control plane. `onlyne-server` 1.0.3 → 1.0.4, `onlyne-client` 1.0.2 →
1.0.3, `onlyne-session` 1.0.1 → 1.0.2, `onlyne-cli` 1.0.1 → 1.0.2, all published
where the crate is on crates.io. `onlyne-proto` stays at 1.0.1: the wire types did
not change, only what the ledger stores in a row it already had.

### Fixed

- server: a `control` row lost its command on the way to the client. The ledger
  keeps no column for `Envelope::control`, so `relay::delivery_from` rebuilt every
  pulled control delivery with `control: None`, and the client's own validation
  refused it: `control kind requires a control op`. `onlyne control … cancel`,
  `recycle`, `probe`, and `snapshot` settled `rejected` and stopped nothing. The
  send path now stores the serialized op in the row body and the rebuild reads it
  back; a row an older server wrote, whose body carries the op name alone, still
  decodes, with the empty reason that row can support. Live case: two `snapshot`
  commands against the same task, both `rejected`, both processes running.
- client: a delivered control command had no consumer. `accept_delivery` staged a
  `control` row exactly as it stages a task, so a cancel aimed at a full role could
  spawn a session instead of freeing one. Control is now applied before the
  `max_sessions` and `accept_new` gates: `recycle` and `cancel` tell the plugin
  where it implements `recycle`, then close the backend resource with
  `CloseReason::Operator` or `Cancelled`; `probe` asks the plugin for a heartbeat
  and republishes the projection; `snapshot` republishes alone. The row settles
  either way, including for a task this role does not hold, because a command no
  client can act on must not stay in flight forever.
- session: the `exec` backend stopped a session by signalling its leader pid
  alone. Every exec session leads its own process group (`process_group(0)` at
  spawn), and the work an agent starts runs inside that group, so a close killed
  the agent and left its driver running under no owner. The close now sends `TERM`
  to the group and, past the grace window, `KILL` to the group, with the single-pid
  send as the fallback when no group answers. Regression: `close_stops_the_children
  _the_session_started` fails against the old `stop` and passes against the new one.
- server: `repair close` and `repair fail` settled the ledger while the work kept
  running. Both now file the same `Cancel` an operator would send by hand, aimed at
  the role that owns the task, through the admin path whose ACL bypass already
  exists for control rows; a role that is offline applies it when its link returns.
  The refusal is logged and never fails the repair: the settlement the operator
  asked for already happened.
- client: a `note` delivery created a session. A note carries no task, so it owns
  none: §5's `note_queue` refuses one whose role is offline, and this is the
  matching half on the receiving side — a note goes to the role's ready session as
  a mid-task message, and a role with no running agent answers `rejected` with
  `note has no live session to wake`. Reported as a three-way disagreement: the
  server filtered queued notes from `pull`, the client rejected a note for naming no
  task, and `onlyne send --note` started a session and made the text its first task.
- cli: a verb that resolved its socket by walking upward chose the wrong
  vocabulary. Both daemons name their socket `.onlyne/run/s`, the walk hardcoded
  `Surface::Client`, and the `--socket` inference keyed on that same suffix, so
  running `onlyne handoff` from inside a server root wrote `query_ledger` into the
  admin socket and got back `unknown op query_ledger` — a verb that exists, on the
  wrong surface. The surface now reads the daemon that owns the tree: `client.db`
  beside the socket is a role workspace, `state.db` is a server root. Field cost:
  the role-side handoff road was closed, and the ring ran on admin-issued
  substitutes.
- cli: `--from`, and `control`'s `--task`/`--to`, had to appear before the verb
  they belong to, and the parse failure only suggested `--as`. They are global
  flags now, so `onlyne control cancel --task X --from bench --reason r` reads the
  way operators type it.

### Known gaps this release leaves open

- The group kill reaches a session's own group. A child that calls `setsid` joins
  a new group and survives; that needs the recorded-peer-pid design below, not a
  wider signal.
- A plugin still does not report whether it consumed a `render_send`.
- `ControlOp` has no message payload, and the client answers a control row with a
  plain ack. A command that reports what it did would let the TUI's `C` key show
  its result inline.

## [1.0.3] - 2026-09-13

Scope: `onlyne-server` only, published to crates.io. `onlyne-client` stays at 1.0.2,
every other crate stays at 1.0.1.

### Fixed

- server: a role-level `pull` re-offered the same in-flight row on every poll,
  forever. The ticket that is supposed to stop that is keyed by the puller's
  session; `relay::pull` armed it with the session row's id when the pull named
  none, so a role-level connection — the ordinary case for a client on TCP —
  could never name its own ticket again and its guard never fired. A row now
  stays handed to the pull that took it, and a session that replaces a dead one
  can still claim it (`State::delivery_ticket`). Live case: one task re-offered
  888 times into the same pi session in three minutes, each round acked `accepted`
  and nothing settling.

## [pi-onlyne 1.1.1] - 2026-09-13

Scope: `plugins/onlyne-agent-pi` only, published to npm. Every crate stays where it is.

### Fixed

- plugin: the injection guard keys the delivery, not the task. A second envelope
  for a running task — a follow-up, a redirect, a bounce back through a relay — is
  a delivery of its own and reaches the model, while the work record it lands on
  keeps its counters and its relay ledger. Keyed by the task, that guard swallowed
  the follow-up and answered `duplicate`, which is what kept the loop above alive:
  an accepted-but-never-completed ack settles nothing.

## [pi-onlyne 1.1.0] - 2026-09-13

Scope: `plugins/onlyne-agent-pi` only, published to npm. Every crate stays at 1.0.2.

### Added

- plugin: an `onlyne` activity widget now carries the routine notices, so a role
  session reads its own message traffic off one panel
  (`src/activity.mjs`: panel key `onlyne`, eight lines capped at 96 columns, six
  events shown of sixty-four kept). The header names role, connection state,
  generation, and the active task with its phase; events are timestamped and
  marked `<=` inbound, `=>` outbound, `!!` warning, `..` state, `~~` duplicate,
  and a run of identical events folds to a trailing ` xN`. `OnlyneAgent.notice` is the single
  funnel: with a widget on the surface it writes the panel, without one it keeps
  the previous channel pair (footer status line plus a `[pi-onlyne]` stderr line).
  `log` stays reserved for the things that need a reader outside the panel —
  relay-guard refusals, socket errors, timeouts, framing faults — and
  `wakeUser`, `proseContext` and the completion exit are untouched.
  `available.widget` gates on `ctx.hasUI` and `ui.setWidget`, so print and JSON
  modes keep the stderr channel.
- plugin: `README` pairs document the panel and the npm install route
  (`pi install npm:pi-onlyne`, `npm:pi-onlyne@<version>` to pin one), and the
  vendored path in the install snippets now matches what `onlyne server generate`
  actually writes — `.onlyne/agent/onlyne-agent-pi`, named from the `agent_package`
  directory (`crates/onlyne-server/src/generate.rs`, `agent_name`).

### Fixed

- plugin: a reconnect names its task in the panel header from the first beat, from
  the same `activeTaskId()` lookup the `assign`-skip decision uses, with the
  assignment still queued behind the handshake.
- tests: the `hello` assertion in `agent.test.mjs` reads the version out of the
  package's own `package.json` (the convention `protocol.test.mjs` already used),
  so a release bump travels once.

## [1.0.2] - 2026-09-13

Scope: `onlyne-client` only. Every other crate stays at 1.0.1.

### Fixed

- client: a `kind = note` send died at both enqueue choke points with
  `internal: outbound envelope missing op_id` (`dispatch::enqueue_outbound` and
  `IntentMachine::enqueue`), even though the proto requires the idempotency key
  only for the non-note kinds (`Envelope::validate`). The client now stamps a
  fresh `op_id` (`onlyne_proto::new_op_id()`) when an outbound envelope carries
  none, validates and queues that stamped copy, and reports its id back to the
  adapter. Notes stay undeduped — every send is its own intent — while a task
  envelope keeps the key it brought, so a re-delivered task still dedups on its
  original id. Field reports: the pi plugin adapter (`protocol.mjs`) mints an
  `op_id` for `task` only — the conformance vector `adapter_plugin_send_note.json`
  pins the keyless note — and the ARIS run log died at 15:13Z on this error while
  sending a note (completion `68222854`).

## [1.0.1] - 2026-09-13

### Fixed

- tui: box corners match their arms, `join()` stopped treating mirrored travel
  orders as equal, and start ticks leave in their own direction (`df78916`).
- This release also carries every fix in the "Fixed after the tag" list under
  `[1.0.0]` below (cli reload socket arm, spec alias matching, tui island
  pruning, plugin manifest pin) into the published crates.

### Changed

- client (**BREAKING**): `onlyne-client start` and `onlyne-client stop` are gone.
  `run` is the only launch verb and it stays in the foreground; backgrounding
  belongs to the operator (a visible terminal tab, `launchd`, `nohup`). The
  client writes no `.onlyne/run/client.pid`, nothing signals it by number, and
  `onlyne client` in `onlyne-cli` forwards no `start`/`stop`.
- client: `status` probes the workspace adapter socket instead of a pid file. A
  client counts as running when that socket answers the admin `hello`, the
  reported uptime is the socket file's mtime age, and the operator line drops
  `pid`: `onlyne: client running uptime <n>s socket <path> faults <n>`. A socket
  file an unclean exit left behind answers nothing, so it reads as not running
  instead of as a live client. `StatusReport` loses `pid` and
  `RoleWorkspace::pid_path` goes with its last consumer.
- docs: the role workspace tree, the client README, the CLI contract, and the
  plan drop `run/client.pid`; `examples/supervisor/run.py` backgrounds
  `onlyne-client run` itself (own session, workspace log, driver-owned pid file)
  and SIGTERMs those pids on `stop`.

## [1.0.0] — tag 396d63f, published to crates.io and npm

Release channel: `cargo install onlyne-cli onlyne-server onlyne-client onlyne-gateway onlyne-tui`
(`onlyne-cli` installs the binary `onlyne`; the other four crates keep their names).
Pi adapter: `pi install npm:pi-onlyne` (1.0.0 speaks wire protocol 1).

### Fixed after the tag, in `main` (carried by the next version bump)

- cli: `onlyne reload` now arms the server unix socket it previously dropped,
  so admin-surface verbs reach a live server started from any cwd.
- config: `spec.toml` accepts `relay_required_count` as an alias of `relay_count`,
  matching the guard file's own spelling; canonical emission and the spec hash
  stay on `relay_count`.
- tui: floating stroke islands are pruned by border connectivity; a route lane
  hidden inside a role box no longer leaves orphan `│`/`─` fragments on the map.
- manifests: every internal path dependency now carries a registry version
  (`cargo publish` requirement); behaviour unchanged.
- repo: the stale `integrations/pi-onlyne` twin (pre-relay-guard code) is deleted;
  `plugins/onlyne-agent-pi` is the one canonical plugin source.

## [Unreleased] — next candidates

- client: record the adapter socket peer pid (kernel `LOCAL_PEERCRED`, not the
  self-reported `mount.pid`) at `hello`, and make `recycle`/`cancel` teardown
  close the backend resource AND kill that pid's group. Field report: a zombie
  `pi` process outlived its pane, reconnected to a fresh client, took a new
  assign, and relaunched its training batch by itself three times — pane dead
  plus ledger settled does not mean the agent process is dead. Counter-case
  (ARIS 2026-09-13): a `repair close` on a frozen projection row (seq 1222,
  orca handle absent) settled the ledger while the process was alive the whole
  time — it sent its handoff and an acked completion minutes after the close.
  A dead projection plane and a dead process look identical from `sessions`;
  only a kernel-known peer pid separates them.
- client: a takeover `hello` (same task_id, different kernel peer pid) bumps
  the session generation; reports carrying a stale generation are rejected
  with `conflict`, so two bodies cannot both write one session's history.
- session: `exec` lands a `setsid`-detached subtree. 1.0.4 signals the group every
  session leads at spawn, which covers the reported case (an agent's own driver
  and training process); a child that calls `setsid` leaves the group and survives
  the close (field report: an ownerless `run_005_resume` stole eight minutes after
  a bypass cancel). Closing that needs a recorded subtree, not a wider signal.
- client: inject `ONLYNE_WORKSPACE` (canonicalised) and base the template
  `session_command` on `cd "$ONLYNE_WORKSPACE"`, ending worktree-mismatch
  double scenes where a relative `cd` resolves in the wrong checkout.
- server: `repair ack` replies echo the owning task/session, so attribution
  survives even though `faults.id` is a per-database autoincrement integer.
- tui/cli: the ledger rows already store the principal's `admin` flag; the
  human projections (ledger table, session view) should render it, so an audit
  read of a control row shows whether the signer acted as admin.
- docs: CLI argument shapes are versioned contracts. `--reason` sits on the
  `cancel`/`recycle` subcommands; `--from` is required on the admin surface
  and rejected on the client surface (`c243348` rule). Any future move of an
  option between top level and subcommand is a breaking change and lands here.
- docs: `repair close` and `repair fail` settle ledger and fault rows and then
  file a `cancel` for the owning role (1.0.4), which is the path that closes the
  resource; `repair adopt|rebind|retry|inspect|ack` still move rows and signal
  nothing. Killing a pane's child by hand stays out of the contract: it skips the
  backend close and its respawn policy, and a terminal-level resurrection follows.
- docs: on a control frame `to` is the executing role (which client carries out
  the recycle/close), defaulting to the signer; a proxy cancel must pass
  `--to <task owner>` explicitly. 1.0.4 settles a command naming a task this role
  does not hold as a delivered no-op, so the ownership *refusal* this line asks
  for is still open, along with an answer that says what the command did.
- server: a push whose frame meets a dead socket learns about it from the next
  `hello`. 1.0.5 requeues a role's in-flight rows when a new link registers,
  which closes the reachability hole; the write that failed still leaves its
  row in flight until a link arrives. A transport failure on the push path
  could roll its own row back to `queued` at the point of failure, which
  shortens the window to zero for a role that never reconnects. Field origin:
  the 13:48 close-out ticket that needed an operator SIGTERM to surface.
- client: one residual sweep per link. The sweep runs detached behind the four
  link loops so a 300-second mount wait cannot stall delivery; a link that
  drops mid-sweep leaves that sweep running until its grace window ends, and
  the next link starts its own. Bounding it needs a cancellation token carried
  by the sweep, and the reports it files stay advisory either way.
