# Changelog

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
