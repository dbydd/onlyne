---
name: onlyne
description: Use when developing the Onlyne repository itself — touching crates, the wire protocol, ledger schema, CLI verbs, e2e proofs, or the verification gates.
---

# Onlyne Development

Guidance for agents changing this codebase. Runtime operation lives in
`skills/onlyne-supervisor/SKILL.md` and `skills/onlyne-role/SKILL.md`; this file covers
working on the repo.

`onlyne skill export [--set role|supervisor|dev]... [--dest DIR] [--force]` writes the four
shipped documents to `<dest>/<name>/SKILL.md`, and `<dest>` defaults to `.agents/skills` under
the working directory: `onlyne-supervisor`, `onlyne-role`, `onlyne-role-payload-v2`, and
`onlyne`, this file, which `--set dev` selects. The bytes are compiled into `onlyne-cli`
(`include_str!` in `crates/onlyne-cli/src/skill.rs`, over the four regular files under
`crates/onlyne-cli/skills/`), so an installed binary answers with the skills of its own version,
over no network and with no checkout. Regular files keep the packaged manuals intact across
checkout and archive tools, and `the_crate_copies_are_the_repository_copies` asserts that every
compiled copy is byte-identical to its source manual. A destination file whose bytes already
match is reported `unchanged` and left alone; a differing file stops the run before any write, with exit 4 and
`onlyne: refusing to overwrite <path>; pass --force`; `--force` rewrites it.

## Product boundary (AGENTS.md §0)

Onlyne is transport: routing, ledger, queueing, ACL, session mechanics. Orchestration
belongs to supervisor sessions and the spec file. The core detects and records; the
operator decides retries, recycling, and timeouts. A feature that starts deciding policy
belongs in a supervisor session, the spec, or a plugin — pick one of those before adding
code here. Zero compatibility is a product rule: old configs, old databases, and old wire
versions fail at the door with a verbatim string. A legacy workspace layout exits 2, a database
whose marker is older exits 1 with `onlyne: unsupported schema; v1.0.0 does not migrate`, and a
protocol revision outside the accepted range earns the wire code `protocol_version`. A change
that "also reads the old shape" gets rejected.

## Crate map and dependency law

```text
onlyne-frame     4-byte BE length + JSON; zero business imports
onlyne-proto     types + validation + error codes; no tokio
onlyne-config    TOML spec/config parsing, env secrets
onlyne-layout    workspace/server root discovery, legacy refusal
onlyne-store     server ledger + client db; (generation,seq) monotonic gates
onlyne-session   pure lifecycle reducer + SessionBackend (herdr|orca|zellij|exec|acp|fake)
onlyne-net       TLS 1.3 + pinning, ed25519 challenge, acl_allows, backoff
onlyne-adapter   the one adapter protocol SDK (agent side and gateway side)
onlyne-acp       ACP v1 client: JSON-RPC over an agent's own stdio
onlyne-tui       the admin-socket observation board the `tui` verb execs
onlyne-server/-client/-gateway   three bins; onlyne-cli the thin entry; onlyne-testkit fakes+e2e
plugins/onlyne-gateway-*         one platform per crate; depend on adapter+proto only
```

`onlyne-server` and `onlyne-client` share no production dependency edge:
`cargo tree -p onlyne-server -e normal` names no `onlyne-client`, and the reverse holds (the name
appears in `crates/onlyne-server/Cargo.toml` under `[dev-dependencies]`, a line no test uses).
Platform SDKs (`teloxide`, `openlark`, `wechat-ilink`, `resvg`) must stay out of both binaries.
After any dependency edit, prove it with
`cargo tree -p onlyne-client | grep -E 'teloxide|openlark|wechat-ilink|resvg'`. Gateways compile
per feature; `--no-default-features --features telegram` must build.

## Change procedures

**Wire or types** (`onlyne-proto`): edit the type, regenerate the schemas
(`cargo run -p onlyne-proto --bin gen-schema`), then update every affected fixture under
`crates/onlyne-proto/tests/wire_vectors/` (one JSON per reachable frame and error code).
Restate the contract in `crates/onlyne-adapter/PROTOCOL.md`. Error codes are a closed set
of fourteen (`ErrorCode::ALL`, `crates/onlyne-proto/src/frame.rs`; fourteen `error_*.json`
fixtures sit beside the frame vectors). Add one and you owe a fixture, a PROTOCOL.md row, and
the regenerated schema that `gen-schema` just wrote.

**Lifecycle** (`onlyne-session/src/lifecycle/`): `apply()` and `is_legal()` are a
table-tested reducer — five axes (`agent`, `delivery`, `resource`, `recovery`, and the
generation's liveness), 20 `LifecycleEvent` variants, versions `(generation, seq)`. A new
transition needs its table rows in the same commit. Reviewers
halt on weakened assertions during a migration.

**Ledger/schema** (`onlyne-store`): `schema_marker(name, version, protocol_version)` is the
gate; this revision writes client 2, server 4 and protocol 1 (`CLIENT_SCHEMA_VERSION`,
`SERVER_SCHEMA_VERSION`), and a database carrying an older marker is refused with
`onlyne: unsupported schema; v1.0.0 does not migrate`. A field change bumps the marker and
leaves the refuse-at-door string untouched.
`acl_allows` runs before the ledger write, so a denied send leaves zero rows and zero
sender-side intents. The built-in exemption covers only completions addressed to the
recorded task origin. Widening it needs a spec decision first.

**CLI verb** (`onlyne-cli`): args live in `verbs.rs`/`admin.rs`/`skill.rs`, out comes one JSON line.
Exit codes, the table `onlyne --help` prints at its foot: `0` answer ok, `1` failed daemon
answer or `wait-ready` bound, `2` validation, `3` no socket, `4` operator refusal (`generate`
input, or `skill export` declining to overwrite), `5` `client run` found no session host,
`127` missing sibling. Socket resolution order stays
`--socket` → `ONLYNE_SOCKET` (the served path the client injects into every session) →
`--server-root` (admin) → `--workspace`/cwd walk (client), and each tree answer resolves
through `socket_path()`. `--from` belongs to
the admin surface only; every message verb already prints JSON.

The `reason` column on a ledger row reaches both read surfaces. `onlyne ledger` projects every
field of the durable row, and `ROW_FIELD_KEYS` (`onlyne-cli/src/ledger.rs`) names `reason` among
the keys a caller reads off it; `task_detail_text` in `onlyne-tui/src/ui.rs` appends
`reason=<text>` to a page-2 row that carries one. `LedgerEntry::reason` (`onlyne-proto/src/ops.rs`)
and `LedgerStateEvent::reason` (`onlyne-proto/src/event.rs`) both serialize `#[serde(default,
skip_serializing_if = "Option::is_none")]`, so a row with nothing to say omits the key and its
bytes match the pre-column shape, and a stored row without the key decodes as no value. The
with-value shape keeps its fixtures under `crates/onlyne-proto/tests/wire_vectors/`
(`res_ledger_answer_rejected_with_reason.json`, `ev_ledger_state_rejected_with_reason.json`).

**Backend** (`onlyne-session/src/backend/`): capabilities `{spawn,attach,probe,close,
focus,rename}`. A missing capability degrades through faults, never panics.
`ONLYNE_BACKEND` names `herdr | orca | zellij | exec | acp | fake | auto`; `headless` parses as
`exec` and projections keep the name `exec` (`BackendName::parse`/`as_str`). Selection order is a
nonempty process `ONLYNE_BACKEND`, then the workspace `config.toml` `backend`, then auto. An empty
value or `auto` probes herdr, then orca, then zellij. `exec`, `acp` and `fake` enable only when one
of those two names them, so auto discovery never picks one. The workspace `[acp]` table
(`AcpSection` in `onlyne-config/src/client.rs`) carries `mode`, `model`, `reasoning_effort` and
`permission` (`deny` default, `allow`), and the ACP backend is its only reader. An ACP session
opens no pane: the client drives the agent with `session/prompt` and reads the streamed
`session/update` notifications, and the conversation lands in
`<workspace>/.onlyne/logs/session-<task>.log` plus `session-<task>.events.jsonl`. No match is `NoSupportedHost`; `onlyne-client run`
exits 5 with a three-line refusal whose first line is
`onlyne: no supported host detected; run inside herdr, orca, or zellij, or set ONLYNE_BACKEND`.
`onlyne-client doctor` prints host-detection JSON and exits 0.
The adapter socket binds `.onlyne/run/s` for a path of 103 bytes or less, and a deeper tree binds the short derived path that `run/socket` records; `run` exits 1 with `onlyne-client: bind the workspace socket <canonical path>: <detail>` when the bind fails — the detail names the served path, both lengths, and the OS reason — and a later `accept` error logs at `error` level and retries every 100 ms.
herdr is kept by operator decision, and its standing lives in
`crates/onlyne-session/src/backend/herdr/NOTE.md`: a workspace that wants another backend names
it in `config.toml`, and a herdr failure carries no product signal.
herdr maps session (inherited) → workspace `onlyne:<cluster>` → tab = role → pane = one onlyne session.
`<cluster>` is the server's `[server] name`, read from `welcome.cluster` and injected into every pane as `ONLYNE_CLUSTER`.
Workspace label `onlyne:<cluster>` and role-name tab are what the backend matches on: a label that differs yields a second workspace, and a tab name that differs yields a second tab.
Before spawning sessions, rename the workspace and tab the backend should use: `herdr workspace rename <WORKSPACE_ID> onlyne:<cluster>` and `herdr tab rename <TAB_ID> <role>`.
The client logs a warning naming the label and the created workspace on the create path.
Spawn: known agent first token → `herdr agent start --kind ... -- <session_command tail>`; remaining commands → `herdr pane run`. `workspace create`/`tab create`/`pane split` pass `--cwd` absolute.
Split: `PanePlacement::from_pane_count`, `(count+1).is_power_of_two()` → `right`, remaining counts → `down`, ratio `0.5`.
Focus: `workspace focus` → `tab focus` → `agent focus <pane_id>` for a managed agent, `pane focus --pane <base_pane> --direction <split_direction>` for a `pane run` shell pane, then `pane get <pane_id>` must report `result.pane.focused`.
`backend_ref` stores `workspace_id`, `tab_id`, `pane_id`, `agent`, `workspace_label`, `base_pane`, `split_direction`.
Control reaches a full role: a client at `max_sessions` pulls with `control_only`, so `focus`/`recycle`/`cancel` land on the session holding the last slot, and task rows stay `queued`.
Retirement invariant an editor keeps: a session's host resource (pane, tab, zellij session, exec child) is closed when the session holds no task and no plugin transport is attached. The client owns the closes (`retire_idle_locked` in `onlyne-client/src/session/dispatch/retire.rs`), and a backend's `close` must stay safe to call on a resource the host already dropped — herdr reads `pane_not_found` as success, debug line `herdr pane already closed`.

**Client dispatch** (`onlyne-client/src/session/dispatch/`, `onlyne-client/src/session/stall.rs`): the dispatch lock serializes slot, transport, backend, and lifecycle work, and the 250 ms readiness tick (`onlyne-client/src/runtime/runloop/`, `READINESS_POLL_MS` in its `config.rs`) drives `reclaim_exited_resources`. A session serves one task for its whole life: the client mints no id of its own, so `session_id` equals `task_id` (`dispatch` in `onlyne-client/src/session/dispatch/delivery.rs`), and a task this role already finished settles from the durable record with no second run (`task_completed_here` in `onlyne-client/src/runtime/runloop/sessions.rs`). `StallWatch::note_applied` refreshes an assigned clock; `note_assigned` owns clock creation, so a late observation from a plugin that already answered cannot reopen a stall episode on a settled task. A connection release forgets the progress clocks of the sessions it served, and both `stall_due` and `stall_report` check the stored lifecycle before a `stalled` fault reaches the wire.

The accept gate decides what a delivery the pull brought meets. A gate shut because the link left `Ready` leaves that row in flight: the client answers nothing, and the next `hello` that does not claim the row is what puts it back on the server's queue (`accept_delivery` in `onlyne-client/src/runtime/runloop/sessions.rs`). The client's own refusal (`accepted: false`) settles a row `rejected`, and that terminal answer is kept for work this client cannot serve at all, such as a pane backend meeting a protocol-speaking command.

A task-bound unsettled session is retired after either a dropped connection exceeds
`[client] reconnect_grace_secs` or an attached transport accepts no frame for three heartbeat
intervals. Both arms settle the bound task `failed`, refuse its delivery row with
`session_dead` (`SESSION_DEAD`, `onlyne-client/src/session/dispatch/retire.rs`), close the host
resource, and publish the exit, so the server's mirrored row reads `exited` in the same tick.

A pane backend refuses a protocol-speaking command before it spawns: `reject_protocol_command_in_pane` runs on `herdr`, `orca`, and `zellij` once the `{session}`/`{task}` tokens are rendered and before `backend.spawn`, and it fires when the argv holds `--acp`, `--mode=rpc`, or `--mode` followed by `rpc`. The correction belongs in the workspace config; an editor that swaps the backend at spawn time hides a mis-set config behind a silent drift, so the delivery fails and the reason reaches the ledger. Nothing opens: no pane, no process, the task row lands `rejected`, and the row's `reason` carries the whole sentence, byte for byte — `{backend} backend cannot host a protocol session: {token} speaks JSON-RPC on its own stdio and the pane would print the frames; set backend = "exec" or backend = "acp" in the workspace config`. The client hands that text to the server as the refusal reason on the delivery's settle intent (`push_settled` in `onlyne-client/src/session/dispatch/slots.rs` → `store_ack` in `onlyne-client/src/session/dispatch/outbound.rs`, answered by `relay::ack`), and the server writes it into the row through `mark_rejected` (`onlyne-store/src/server.rs`), which is where the operator reads it.

## Gates

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                 # per-crate -p reruns suffice for isolated edits
crates/onlyne-testkit/e2e/<case>.sh    # ONLYNE_BACKEND=fake; ONLYNE_BIN_DIR picks the build dir
```

The eighteen case scripts under `crates/onlyne-testkit/e2e/` each encode one verification case,
and the directory keeps `lib.sh`, the shared harness, beside them plus `acp-agent.py`, the
scripted ACP peer cases 18 and 19 drive. The named cases cover the single-machine task (1), ACL
rejects (2), idempotency (3), reconnect requeue (4), two-cluster federation (5), gateway mount
(6), legacy refusal (7), relocation (9), the running-lights ring (12), the heartbeat watch (14),
the hello claim across a server restart (15), the headless `exec` face (16), the deep-workspace
socket `socket-path-length.sh` (17), the ACP backend `acp-session.sh` (18), and the payload-v2
report `acp-payload-v2.sh` (19); the live-host faces are Orca (10), pi (11), and herdr (13).
Cases 1-7, 9, 12, and 14-17 run on `ONLYNE_BACKEND=fake`, and cases 18 and 19 name
`backend = "acp"` with that scripted agent. Case 8 of the plan is the static gate above, which
is why no script carries its number.

The final release-window e2e record in `docs/live-evidence-1.4.0.md` reports 19/19 scripts
green. The release commit's local workspace gate in `Devlogs.md` reports 1101 passed, 0 failed,
and 1 ignored across 69 targets.

The earlier 2026-09-23 sweep from the repository root with `ONLYNE_BIN_DIR=target/release`
(`lib.sh` derives `BIN_DIR` from `ONLYNE_BIN_DIR`, and `target/debug` is its default) was:

```text
local-task 0            acl-reject 0          idempotency 0       reconnect-requeue 0
two-cluster 0           gateway-mount 0       legacy-layout 0     generate-relocate 0
heartbeat-watch 0       requeue-claim 0       exec-headless 0     socket-path-length 0
acp-session 0           orca-live 0           pi-live 1           running-lights 0
acp-payload-v2 0        herdr-live 0 (SKIP)
```

This dated sweep preceded the final release fixes. Its result remains historical evidence; the
final 19/19 record and the separate 1101/69 release gate above are the current readings.

A bug fix needs its reproduction as an e2e or a table test:
red before the fix, green after. The live ring demo
(`examples/supervisor/run.py`) needs a real `pi` on PATH: inside an Orca tab its sessions take
tabs, and outside one `ONLYNE_BACKEND=exec` runs them headless. Treat it as manual smoke.

Socket invariant: the path a daemon binds is the path `socket_path()` returns, and every
finder — CLI, TUI, fake agent, plugin — resolves through `onlyne-layout`
(`SocketEndpoint`/`socket_path()`/`bind_socket`). An edit that joins `.onlyne/run/s` by hand
splits a deep workspace in two: the canonical spelling stays bare, the daemon serves a
short derived path, and `run/socket` names the served one.

## Formal invariants

`proofs/` (GrugMatic, core Lean 4.33.1, zero dependencies) carries one combinator lemma per
design decision: D4 carrier bounds, D5 authority split, D6 content by reference, D11
idempotence, D12 delivery-creates-task, D13 file truth, D15 single-source prose, one-shot
sessions. When a change touches one of those invariants, read the lemma's docstring first.
If the change breaks the lemma, update `proofs/` in the same commit and keep
`cd proofs && lake build` green with zero `sorry`.
