---
name: onlyne
description: Use when developing the Onlyne repository itself — touching crates, the wire protocol, ledger schema, CLI verbs, e2e proofs, or the verification gates.
---

# Onlyne Development

Guidance for agents changing this codebase. Runtime operation lives in
`skills/onlyne-supervisor/SKILL.md` and `skills/onlyne-role/SKILL.md`; this file covers
working on the repo.

## Product boundary (AGENTS.md §0)

Onlyne is transport: routing, ledger, queueing, ACL, session mechanics. Orchestration
belongs to supervisor sessions and the spec file. The core detects and records; the
operator decides retries, recycling, and timeouts. A feature that starts deciding policy
belongs in a supervisor session, the spec, or a plugin — pick one of those before adding
code here. Zero compatibility is a product rule: old configs, old databases, and old wire
versions fail at the door (`exit 2`, verbatim strings). A change that "also reads the old
shape" gets rejected.

## Crate map and dependency law

```text
onlyne-frame     4-byte BE length + JSON; zero business imports
onlyne-proto     types + validation + error codes; no tokio
onlyne-config    TOML spec/config parsing, env secrets
onlyne-layout    workspace/server root discovery, legacy refusal
onlyne-store     server ledger + client db; (generation,seq) monotonic gates
onlyne-session   pure lifecycle reducer + SessionBackend (herdr|orca|zellij|exec|fake)
onlyne-net       TLS 1.3 + pinning, ed25519 challenge, acl_allows, backoff
onlyne-adapter   the one adapter protocol SDK (agent side and gateway side)
onlyne-server/-client/-gateway   three bins; onlyne-cli the thin entry; onlyne-testkit fakes+e2e
plugins/onlyne-gateway-*         one platform per crate; depend on adapter+proto only
```

`onlyne-server` and `onlyne-client` never depend on each other. Platform SDKs
(`teloxide`, `openlark`, `wechat-ilink`, `resvg`) must stay out of both binaries. After any
dependency edit, prove it with
`cargo tree -p onlyne-client | grep -E 'teloxide|openlark|resvg'`. Gateways compile per
feature; `--no-default-features --features telegram` must build.

## Change procedures

**Wire or types** (`onlyne-proto`): edit the type, regenerate the schemas
(`cargo run -p onlyne-proto --bin gen-schema`), then update every affected fixture under
`crates/onlyne-proto/tests/wire_vectors/` (one JSON per reachable frame and error code).
Restate the contract in `crates/onlyne-adapter/PROTOCOL.md`. Error codes are a closed set
of fourteen. Add one and you owe a fixture, a PROTOCOL.md row, and the CLI table below.

**Lifecycle** (`onlyne-session/src/lifecycle.rs`): `apply()` and `is_legal()` are a
table-tested reducer — five state axes, 21 `LifecycleEvent` variants, versions
`(generation, seq)`. A new transition needs its table rows in the same commit. Reviewers
halt on weakened assertions during a migration.

**Ledger/schema** (`onlyne-store`): `schema_marker(name, version, protocol_version)` is the
gate. A field change bumps the marker and leaves the refuse-at-door string untouched.
`acl_allows` runs before the ledger write, so a denied send leaves zero rows and zero
sender-side intents. The built-in exemption covers only completions addressed to the
recorded task origin. Widening it needs a spec decision first.

**CLI verb** (`onlyne-cli`): args live in `verbs.rs`/`admin.rs`, out comes one JSON line.
Exit codes: `0` answer ok, `1` failed daemon answer or `wait-ready` bound, `2` validation,
`3` no socket, `4` generate refusal, `127` missing sibling. Socket resolution order stays
`--socket` → `ONLYNE_SOCKET` (the served path the client injects into every session) →
`--server-root` (admin) → `--workspace`/cwd walk (client), and each tree answer resolves
through `socket_path()`. `--from` belongs to
the admin surface only; every message verb already prints JSON.

**Backend** (`onlyne-session/src/backend/`): capabilities `{spawn,attach,probe,close,
focus,rename}`. A missing capability degrades through faults, never panics.
`ONLYNE_BACKEND` names `herdr | orca | zellij | exec | fake | auto`. An empty value
or `auto` probes herdr, then orca, then zellij. `exec` and `fake` enable only when
`ONLYNE_BACKEND` names them. No match is `NoSupportedHost`; `onlyne-client run`
exits 5 with `onlyne: no supported host detected; run inside herdr, orca, or zellij, or set ONLYNE_BACKEND`.
`onlyne-client doctor` prints host-detection JSON and exits 0.
The adapter socket binds `.onlyne/run/s` while the path fits 103 bytes, and a deeper tree binds the short derived path that `run/socket` records; `run` exits 1 with `onlyne-client: bind the workspace socket <canonical path>: <detail>` when the bind fails — the detail names the served path, both lengths, and the OS reason — and a later `accept` error logs at `error` level and retries every 100 ms.
herdr maps session (inherited) → workspace `onlyne:<cluster>` → tab = role → pane = one onlyne session.
`<cluster>` is the server's `[server] name`, read from `welcome.cluster` and injected into every pane as `ONLYNE_CLUSTER`.
Workspace label `onlyne:<cluster>` and role-name tab are what the backend matches on: a label that differs yields a second workspace, and a tab name that differs yields a second tab.
Before spawning sessions, rename the workspace and tab the backend should use: `herdr workspace rename <WORKSPACE_ID> onlyne:<cluster>` and `herdr tab rename <TAB_ID> <role>`.
The client logs a warning naming the label and the created workspace on the create path.
Spawn: known agent first token → `herdr agent start --kind ... -- <session_command tail>`; remaining commands → `herdr pane run`. `workspace create`/`tab create`/`pane split` pass `--cwd` absolute.
Split: `PanePlacement::from_pane_count`, `(count+1).is_power_of_two()` → `right`, remaining counts → `down`, ratio `0.5`.
Focus: `workspace focus` → `tab focus` → `agent focus <pane_id>` for a managed agent, `pane focus --pane <base_pane> --direction <split_direction>` for a `pane run` shell pane, then `pane get <pane_id>` must report `result.pane.focused`.
`backend_ref` stores `workspace_id`, `tab_id`, `pane_id`, `agent`, `workspace_label`, `base_pane`, `split_direction`.
Control reaches a full role: a client at `max_sessions` pulls with `control_only`, so `focus`/`recycle`/`cancel` land on the session holding the last slot while task rows stay `queued`.
Retirement invariant an editor keeps: a session's host resource (pane, tab, zellij session, exec child) is closed when the session holds no task and no plugin transport is attached. `reuse` survives only while the agent is attached. The client owns the closes (`retire_idle_locked` in `onlyne-client/src/dispatch.rs`), and a backend's `close` must stay safe to call on a resource the host already dropped — herdr reads `pane_not_found` as success, debug line `herdr pane already closed`.

**Client dispatch** (`onlyne-client/src/dispatch.rs`, `onlyne-client/src/stall.rs`): the dispatch lock serializes slot, transport, backend, and lifecycle work, and the 250 ms readiness tick (`runloop.rs`) drives `reclaim_exited_resources`. `StallWatch::note_applied` refreshes an assigned clock; `note_assigned` owns clock creation, so a late observation from a plugin that already answered cannot reopen a stall episode on a settled task. A connection release forgets the progress clocks of the sessions it served, and both `stall_due` and `stall_report` check the stored lifecycle before a `stalled` fault reaches the wire.

## Gates

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                 # per-crate -p reruns suffice for isolated edits
crates/onlyne-testkit/e2e/<case>.sh    # ONLYNE_BACKEND=fake, built target/debug, no real creds
```

The sixteen scripts under `crates/onlyne-testkit/e2e/` each encode one verification case
from `docs/v1-PLAN.md` (ACL rejects, idempotency, reconnect requeue, hello claim across a
server restart, gateway mount, relocation, two-cluster federation, legacy refusal, frame
bounds, and the deep-workspace socket `socket-path-length.sh`). A bug fix needs its
reproduction as an e2e or a table test: red before the fix, green after. The live ring demo
(`examples/supervisor/run.py`) needs Orca and real pi binaries. Treat it as manual smoke.

Socket invariant: the path a daemon binds is the path `socket_path()` returns, and every
finder — CLI, TUI, fake agent, plugin — resolves through `onlyne-layout`
(`SocketEndpoint`/`socket_path()`/`bind_socket`). An edit that joins `.onlyne/run/s` by hand
splits a deep workspace in two: the canonical spelling stays bare while the daemon serves a
short derived path, and `run/socket` names the served one.

## Formal invariants

`proofs/` (GrugMatic, core Lean 4.33.1, zero dependencies) carries one combinator lemma per
design decision: D4 carrier bounds, D5 authority split, D6 content by reference, D11
idempotence, D12 delivery-creates-task, D13 file truth, D15 single-source prose, one-shot
sessions. When a change touches one of those invariants, read the lemma's docstring first.
If the change breaks the lemma, update `proofs/` in the same commit and keep
`cd proofs && lake build` green with zero `sorry`.
