# Onlyne Status

v1.0.0 is the active state. `docs/v1-PLAN.md` is the settled spec. `docs/v1-CONTRACT.md` owns the work split. Root README files are the user manual.

## Three-process shape

- `onlyne-server` routes envelopes and holds the ledger.
- `onlyne-client` owns one role workspace and its session execution.
- `onlyne-gateway` translates one chat platform through a feature-gated plugin.
- Agent plugins and gateway plugins use one adapter protocol over two mount kinds.

## Crate state

Counts come from `cargo test --workspace` on 2026-09-11 (496 passed, 0 failed), one line per crate with its libraries and integration targets summed.

- [x] `onlyne-proto` green with envelope, frame variants, ops, errors, and events: 57 unit + 5 wire vectors + 1 sizes.
- [x] `onlyne-frame` green with length-prefixed codec: 9.
- [x] `onlyne-config` green with spec parse and reload: 11 template + 19 config contract + 17 ACL table + 3 spec example.
- [x] `onlyne-layout` green with legacy refusal exit 2: 15.
- [x] `onlyne-store` green with ledger and local DB: 24 unit + 2 schema statements.
- [x] `onlyne-session` green with lifecycle port and the Orca backend: 49.
- [x] `onlyne-net` green with TLS, handshake, ACL, and backoff: 25.
- [x] `onlyne-adapter` green with SDK and protocol schema: 5 unit + 3 conformance + 1 protocol doc.
- [x] `onlyne-server` green with router, relay, projection, faults, admin, and generate: 48 delivery + 26 generate.
- [x] `onlyne-client` green with runloop, intents, adapter socket, and dispatch: 15 unit + 21 scenarios.
- [x] `onlyne-tui` green with the role network graph and the swarm monitor: 15.
- [x] `onlyne-gateway` green with shared kit: 47.
- [x] `onlyne-cli` green with entrypoint and socket resolution: 21.
- [x] `onlyne-testkit` green with fake agent, fake gateway, and conformance: 2 binaries + 11 conformance.
- [x] Four gateway plugins green behind `telegram`, `feishu`, `qqbot`, and `weixin` features: 11, 10, 10, 13.

## Wave plan status

- [x] Wave 1 closed: proto, frame, session kernel, config/layout/store, net.
- [x] Wave 2 closed: server runtime, client runtime, adapter SDK plus testkit, gateway kit.
- [x] Wave 3 closed: generate, federation path, legacy deletion, docs.

## Verification cases

Each case is a script under `crates/onlyne-testkit/e2e/`, run with `ONLYNE_BACKEND=fake BIN_DIR=target/debug` from the repository root. All nine exited 0 on 2026-09-11, the eight fake-backend cases in 15 seconds together and case 10 in 8; case 10 overrides the backend and needs a shell inside an Orca tab with the app answering, and skips itself (exit 0) otherwise.

- [x] Case 1 `local-task.sh`: single-machine fake-backend task reaches `acked`.
- [x] Case 2 `acl-reject.sh`: ACL refusal emits `acl_denied`.
- [x] Case 3 `idempotency.sh`: repeated `op_id` emits `duplicate`; changed body emits `conflict`.
- [x] Case 4 `reconnect-requeue.sh`: disconnect keeps queue state and reconnect flushes in order.
- [x] Case 5 `two-cluster.sh`: aggregate-role federation preserves the parent ledger boundary.
- [x] Case 6 `gateway-mount.sh`: gateway mount delivers platform traffic.
- [x] Case 7 `legacy-layout.sh`: legacy workspace exits 2.
- [x] Case 8: formatting, lint, workspace tests, and binary firewall checks pass.
- [x] Case 9 `generate-relocate.sh`: generate produces relocatable workspaces.
- [x] Case 10 `orca-live.sh`: the Orca backend against the live app — the tab landing flat in the supervisor's own worktree under the `host` policy, four-part probe inputs, the tab map carrying that identity, SIGTERM drain back to the tab count it started with, and no Orca registration created.
