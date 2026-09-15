# GOAL — 1.0.9: delivery truth — hello claims, loud in_flight, requeue budget, stall report

## Objective
A reconnecting client that still runs a task keeps exactly one session on it: the hello handshake declares live tasks, adoption requeue leaves those rows `in_flight` with tickets rebound to the new link, every delivery-state flip publishes a `ledger_state` event so the ledger never silently contradicts the session mirror, automatic requeue honors a spec budget (`requeue_max_attempts`, `requeue_ttl_secs`), and a running session whose projection tuple freezes past a client-side threshold reports a `stalled` fault once per episode.

## Scope
- proto: `HandshakeArgs.live_tasks: Vec<String>`, serde default, wire-compatible with older servers and clients (done on main thread, `crates/onlyne-proto/src/ops.rs`).
- server: claim-aware `relay::requeue_role_rows` + ticket rebind at hello (`router.rs`), `ledger_state` event on the pull path's `in_flight` flip (`relay.rs`), requeue guard terminal states `requeue_exhausted` / `requeue_ttl` (`relay.rs`).
- store: `ledger.requeued` column via in-place `ensure_` ALTER, increment inside `requeue_one` (`crates/onlyne-store/src/server.rs`).
- config: `[server] requeue_max_attempts` (u32, 0 = unlimited), `requeue_ttl_secs` (u64, 0 = off); client `stall_report_secs` (u64, default 1800, 0 = off).
- client: fill `live_tasks` from running non-stale store rows at hello; per-task progress anchor refreshed on real `Applied` tuples only; `stalled` fault report with episode dedup on the existing `Report::Fault` path.
- docs: operations (new knobs + requeue section), PROTOCOL/AGENTS §6 unchanged vocabularies, CHANGELOG 1.0.9, STATUS, skill, example configs; then release: versions server 1.0.9 / client 1.0.7 / store 1.0.5 / proto 1.0.4 / config 1.0.4 / tui 1.0.4 / workspace 1.0.4, dependency floors raised to real minimums, publish six crates, install to `~/.cargo/bin`.

## Out of scope
- pi-side retry budgets (user order 2026-09-15: each host owns its own retries).
- Any server policy that closes, restarts, or auto-repairs a session: recovery stays supervisor + `repair_*` (AGENTS §11).
- New verbs, ErrorCode members, exit-code changes, schema-marker bumps.
- Gateway/TUI feature work; TUI only shows what existing rows and faults already carry.

## Done when
1. A hello with `live_tasks` containing a task keeps that task's `in_flight` row untouched, ticket generation names the new link, and the new link's teardown requeues it later; a hello without the field behaves exactly as before.
2. A pull that hands a queued row publishes the same-shaped `ledger_state` `in_flight` event the push path publishes.
3. With `requeue_max_attempts = N`, the (N+1)-th automatic requeue terminalizes the row (`failed`, reason `requeue_exhausted`) with its event; `requeue_ttl_secs` terminalizes via `expired`/`requeue_ttl`; both default off and byte-identical to 1.0.8.
4. A running session with no `Applied` tuple change for `stall_report_secs` emits exactly one `stalled` fault per freeze episode; the next `Applied` re-arms; `0` disables.
5. Gates green: fmt, clippy, `cargo test --offline --workspace`, firewall edges 0, ten fake e2e cases + release build; commit pushed; six crates published; `~/.cargo/bin` carries 1.0.9 binaries.
6. CHANGELOG 1.0.9 + STATUS + operations match shipped behavior; ARIS reply covers the verified mechanism (adoption requeue + silent pull flip), the morning double-open answer, and the release.
