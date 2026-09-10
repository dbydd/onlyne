# Onlyne v1.0.0 — internal work-split contract

Read this before touching code. The director owns commits, the root `Cargo.toml`, and scope changes.

## Repo state

Cargo workspace, `members = ["crates/*", "plugins/*"]`, 18 packages. `crates/onlyne-legacy/` holds the pre-v1 daemon; it is read-only reference material and gets deleted in S12. `vendor/harness/onlyne-swarm/src/` is a read-only snapshot of the deleted orchestrator submodule: the source of the session kernel being ported.
`docs/v1-PLAN.md` is the full design spec (527 lines, Chinese). Read the section your task names before writing code; it wins over a worker brief wherever they disagree, with file ownership as the exception.

## Ownership

| owned paths | deliverable |
|---|---|
| `crates/onlyne-frame/`, `crates/onlyne-proto/` | length-prefixed JSON codec, wire types, schema export |
| `crates/onlyne-session/` | lifecycle reducer, `SessionBackend`, ledger bridge |
| `crates/onlyne-config/`, `crates/onlyne-layout/`, `crates/onlyne-store/` | spec TOML, workspace layout, SQLite |
| `crates/onlyne-net/` | TLS, ed25519 handshake, ACL, backoff |
| `crates/onlyne-adapter/`, `crates/onlyne-testkit/` | adapter SDK, fake agent/gateway, conformance |
| `crates/onlyne-gateway/`, `plugins/*` | gateway kit, four platform plugins |
| `crates/onlyne-server/` | router, relay, projection, faults, admin, generate |
| `crates/onlyne-client/` | runloop, adapter socket, accept, dispatch, intent |
| `crates/onlyne-cli/` | thin human entrypoint |

## Hard rules

- Touch only your owned paths. A file another worker owns is read-only for you.
- Never edit the root `Cargo.toml`. When a dependency is missing from `[workspace.dependencies]`, declare it in your own `Cargo.toml` with an explicit version and list it in your report for consolidation.
- Never run `git`. Never delete files outside your owned paths.
- Use a private target directory so builds do not serialise on the shared lock: `CARGO_TARGET_DIR=target/<name> cargo check -p <crate>`. Crate-scoped cargo commands only.
- Zero backward compatibility: no legacy aliases, no dual read paths, no `#[allow(dead_code)]` keeping unused ported code, no `todo!()`, no stub returning `Ok(())` pretending to work.
- Byte-exact strings: when the brief quotes a user-facing message, reproduce it character for character.
- Comments and docs use direct additive sentences. Forbidden rhetoric includes `not X but Y`, `rather than`, `instead of`, `however`, `but`, `on the other hand`, and every equivalent in Chinese.
- Report files touched with line counts, the exact commands run, their real output tail, and any gap with its reason. A test claim without runner output is a gap.

## Cross-crate decoupling decisions

- `onlyne-frame` carries codec only. `onlyne-proto` carries all wire types and holds no tokio.
- `onlyne-proto` public API is the single source of names: `Envelope`, `Body`, `ImagePart`, `Causality`, `Principal`, `MsgKind`, `ControlOp`, `Outcome`, `Frame`, `ResBody`, `ErrorPayload`, `ErrorCode`, `Event`, `LedgerState`, `Presence`, `GatewayHealth`, `Lifecycle`, `EventTier`, `ClientOp`, `AdminOp`, `GatewayOp`, `Report`, `SessionProjection`, `Receipt`, `Welcome`, `HandshakeArgs`, `PluginOp`, `HostOp`, `AdapterMsg`, `Capability`, `Mount`, `HelloArgs`, `HelloAck`. Exact field lists live in `crates/onlyne-proto/src/*.rs`; read them.
- `onlyne-net` does not depend on `onlyne-config`. It owns `RoleAcl`, `AclTable`, `MsgClass`, and `acl_allows(table, from, to, class, owner: Option<&str>) -> Result<(), AclDeny>`, keyed by role-name strings, while `onlyne-config` owns `Spec` and its wildcard expansion.
- Deviation from `docs/v1-PLAN.md` §5 line 274, recorded here: the plan writes `acl_allows(spec, from, to, kind)` in `onlyne-net`, and the shipped shape is `Spec::acl_edges()` in `crates/onlyne-config/src/spec.rs` plus `AclTable::acl_allows` in `crates/onlyne-net/src/acl.rs`.
- The wildcard expands once, at load, inside `Spec::acl_edges()`, which is the single place `"*"` carries meaning.
- `onlyne-net` stores concrete role names, and `AclTable::new` refuses a `"*"` endpoint as a wiring bug.
- The ordering rule from line 274 holds: `crates/onlyne-server/src/relay.rs` calls `AclTable::acl_allows` before the ledger append, and the server translates `AclDeny { reason, field }` into `ErrorCode`.
- The reason is the dependency direction in this section: `Spec` is a TOML document with `deny_unknown_fields`, line-number diagnostics, `spec_hash` canonicalisation, and a reload diff, and none of that belongs on the wire. The semantics as data live in `crates/onlyne-config/tests/acl_table.rs`.
- `onlyne-session` does not depend on `onlyne-proto`, `onlyne-store`, or `onlyne-net`. Persistence crosses the `SessionLedger` trait defined in `crates/onlyne-session/src/reconcile.rs`.
- `onlyne-store` implements `onlyne-session::SessionLedger` for the client database, and exposes the server ledger API described in §10 of the plan.
- JSON Schemas live beside their types: `crates/onlyne-proto/schema/{envelope,adapter}.schema.json`, `crates/onlyne-config/schema/{spec,config-client}.schema.json`. Two `gen-schema` binaries, one per crate.

## CLI vocabulary (`onlyne-cli`, output is JSON by default)

```
onlyne server start|stop|run|init|reload|generate|status|roles|sessions|ledger|faults|watch|history|repair ...   # execs onlyne-server
onlyne client run|start|stop|status|init|roles|sessions|watch|history                                            # execs onlyne-client
onlyne status|roles|sessions|ledger|faults|watch|history|spec_diff|reload|generate|wait-ready|repair ...          # admin surface, one frame per call
onlyne cluster export-prose
onlyne send --to <role> [--task <id>] [--text ...|--file -] [--image f.png] [--note]
onlyne reply --to <envelope-id> --text ...
onlyne complete --task <id> [--outcome done|failed|cancelled] --text ...
onlyne handoff --to <role> --task <id> --text ...
onlyne control recycle|probe|snapshot|cancel --task <id> [--reason ...]
onlyne gateway run <telegram|feishu|qqbot|weixin> --server-root <dir> [--token ...]
onlyne gateway list|status|auth <platform> [...]
onlyne who|ping|version|completions <zsh|fish>
```

Three daemons plus one entrypoint: `onlyne-server`, `onlyne-client`, `onlyne-gateway`, `onlyne`. `onlyne <group> <verb>` execs the matching daemon binary; message verbs connect straight to a unix socket and print one JSON answer.
The admin noun set resolves through the same socket rule and shares the message-verb exit-code table; `wait-ready` polls admin `status` at 200 ms intervals with a 10 s bound and prints `onlyne: server not ready after 10000ms` on failure; `generate` writes the `[[client]]` fragment to stdout and progress to stderr.
A missing daemon binary makes the CLI and the e2e script print exactly this to stderr and exit 127:
`onlyne: missing binary <path>; run cargo build --workspace`
`onlyne cluster export-prose` prints raw prose by default and takes `--json`. It issues the existing role query and adds no protocol op.

Socket resolution, in this order: `--socket <path>` → `--server-root <dir>` as `<dir>/.onlyne/run/s` → `--workspace <dir>` or the current directory upward for `.onlyne/run/s`. None found writes exactly this to stderr and exits 3:

```
onlyne: no onlyne socket found; pass --socket, --server-root, or --workspace
```

## Wave order

1. proto + frame finish, session kernel, config/layout/store, net. These four are independent.
2. server runtime, client runtime, adapter SDK + testkit, gateway kit.
3. generate, federation path, legacy deletion, docs.

## Process verbs versus admin queries

`onlyne-server` owns the process verbs `init`, `run`, `start`, `stop`, `status`, `generate`, and `reload`; `reload --dry-run` prints `SpecDiff::render()`.
`onlyne server roles|sessions|ledger|faults|watch|history|repair_*` stays in `onlyne-cli`, which resolves those verbs against the admin socket and formats the answers for a human, with no exec of `onlyne-server`.
`onlyne-server status` answers the process question from its own tree: pid, socket path, uptime, spec hash, and store reachability.
`onlyne status` on the CLI is the `AdminOp::Status` socket round-trip.
The two answer different questions: one describes the local process, the other describes the live cluster.
`onlyne client` and `onlyne gateway` follow the same split: each daemon binary owns `run`, `start`, `stop`, and its own `status`, and `onlyne-cli` resolves every query verb against that daemon's socket.
Prose keeps direct additive sentences; contrastive rhetoric stays banned.
