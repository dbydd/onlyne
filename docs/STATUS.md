# Onlyne Status

v1.0.0 is the active state. `docs/v1-PLAN.md` is the settled spec. `docs/v1-CONTRACT.md` owns the work split. Root README files are the user manual.

## Three-process shape

- `onlyne-server` routes envelopes and holds the ledger.
- `onlyne-client` owns one role workspace and its session execution.
- `onlyne-gateway` translates one chat platform through a feature-gated plugin.
- Agent plugins and gateway plugins use one adapter protocol over two mount kinds.

## Crate state

- [ ] `onlyne-proto` green with envelope, frame variants, ops, errors, and events test count recorded here.
- [ ] `onlyne-frame` green with length-prefixed codec test count recorded here.
- [ ] `onlyne-config` green with spec parse and reload test count recorded here.
- [ ] `onlyne-layout` green with legacy refusal exit 2 test count recorded here.
- [ ] `onlyne-store` green with ledger and local DB test count recorded here.
- [ ] `onlyne-session` green with lifecycle port test count recorded here.
- [ ] `onlyne-net` green with TLS, handshake, ACL, and backoff test count recorded here.
- [ ] `onlyne-adapter` green with SDK and protocol schema test count recorded here.
- [ ] `onlyne-server` green with router, relay, projection, faults, admin, and generate test count recorded here.
- [ ] `onlyne-client` green with runloop, intents, adapter socket, and dispatch test count recorded here.
- [ ] `onlyne-gateway` green with shared kit test count recorded here.
- [ ] `onlyne-cli` green with entrypoint and socket resolution test count recorded here.
- [ ] `onlyne-testkit` green with fake agent, fake gateway, and conformance test count recorded here.
- [ ] Four gateway plugins green behind `telegram`, `feishu`, `qqbot`, and `weixin` features.

## Wave plan status

- [ ] Wave 1 closed: proto, frame, session kernel, config/layout/store, net.
- [ ] Wave 2 closed: server runtime, client runtime, adapter SDK plus testkit, gateway kit.
- [ ] Wave 3 closed: generate, federation path, legacy deletion, docs.

## Open verification cases

- [ ] Case 1: single-machine fake-backend task reaches `acked`.
- [ ] Case 2: ACL refusal emits `acl_denied`.
- [ ] Case 3: repeated `op_id` emits `duplicate`; changed body emits `conflict`.
- [ ] Case 4: disconnect keeps queue state and reconnect flushes in order.
- [ ] Case 5: aggregate-role federation preserves the parent ledger boundary.
- [ ] Case 6: gateway mount delivers platform traffic.
- [ ] Case 7: legacy workspace exits 2.
- [ ] Case 8: formatting, lint, workspace tests, and binary firewall checks pass.
- [ ] Case 9: generate produces relocatable workspaces.
