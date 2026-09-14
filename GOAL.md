# GOAL — 1.0.8: server-side heartbeat supervision closes the lifecycle desync (ARIS night report 2026-09-15)

## Objective
A dead session process stops being invisible: every plugin heartbeat keeps the server row fresh, the server flags a silent `working` row with a `heartbeat_missing` fault and a derived stale answer regardless of role online state, a `working` row revived by a post-completion heartbeat faults `heartbeat_after_complete`, and the TUI plus `onlyne sessions` show stale instead of live.

## Scope
- client: publish liveness on every accepted plugin beat, no-op tuples included (`crates/onlyne-client/src/dispatch.rs`, store version bump).
- server: `heartbeat_grace_secs` sweep on the existing stale watch; seen-since-open gate against restart races; revival fault in `projection.rs`; derived `heartbeat_stale` on session answers (`stale.rs`, `projection.rs`, `router.rs`/`admin.rs`, `lib.rs`).
- config: `[server] heartbeat_grace_secs`, default 90 (`crates/onlyne-config`).
- proto: `SessionRow.heartbeat_stale: bool`, serde default (`crates/onlyne-proto`), size vectors updated.
- tui: stale rendering (`crates/onlyne-tui`).
- docs: operations.md stale section, PROTOCOL.md line 73 gets the host-liveness clarification note, CHANGELOG 1.0.8 + versions, STATUS counts.

## Out of scope
- Closing the pane on completion: `release_locked(…, None)` in `on_out` is deliberate idle-slot reuse (§5 `reuse`, dispatch.rs:1114-1117). Untouched.
- Orca pane polling or any new orca liveness path (user order 2026-09-15: liveness travels on the plugin heartbeat, not on orca).
- Auto-repair, auto-respawn, lifecycle flips written by the server: the mirror stays client-authored; recovery stays supervisor + `repair_*` (AGENTS §11).
- zellij/herdr probe loops: unchanged.

## Done when
1. A no-op beat advances the stored seq and publishes one `session_sync` per beat; a stale or illegal beat does not.
2. Server with an online role and a working row older than `heartbeat_grace_secs` records one open `heartbeat_missing` fault and emits its event; a fresh row does not; a row never seen since server open does not.
3. An accepted write that moves an `exited` row back to `working` inside one generation records `heartbeat_after_complete` and keeps the mirror truth.
4. `onlyne sessions --json` answers carry `heartbeat_stale`; TUI shows the state on the session axis.
5. Gates green: fmt, clippy, `cargo test --offline --workspace`, firewall edges 0, nine fake e2e cases.
6. CHANGELOG 1.0.8 + STATUS counts + operations doc match the shipped behavior.
