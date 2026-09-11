# Onlyne Federation Design

Protocol note for v1.0.0. Federation here means reaching one cluster from another cluster through the mechanisms every local participant uses. v1.0.0 adds no federation runtime code. Sources: `docs/v1-PLAN.md` S11 lines 458-463, decision D14 at line 28, decision D15 at line 29, verification case 5 at line 504.

## The recursion claim

A child cluster reaches a parent cluster through the mechanisms any local participant uses. The supervisor's client connects to the parent server with the aggregate role's public key registered in the parent spec, and it presents the aggregate name as the role it serves. Source: `docs/v1-PLAN.md` S11 line 460.

> supervisor 的 client 用「父 spec 中登记的 aggregate role 公钥」连父 server，`hello.args.mount.role` 填 aggregate 名。

That connection is the single implementation point of the whole recursion path, and the protocol gains zero federal ops. Source: `docs/v1-PLAN.md` S11 line 458; decision D14 at line 28.

The landed client-to-server handshake carries that identity in `HandshakeArgs`. Its `role` field is "the role this connection serves, or the aggregate role it represents", `key` is the `ed25519/<base64>` public key the connection presents, and `aggregate` is true for a connection serving a sub-cluster. Source: `crates/onlyne-proto/src/ops.rs` lines 396-409; `crates/onlyne-net/src/handshake.rs` lines 41-56 and 296-301.

The server accepts that connection against the ACL table built from its own spec, and refuses a key that differs from the registered one. Source: `role_listener` in `crates/onlyne-server/src/lib.rs` lines 66-89; `handshake::accept` in `crates/onlyne-net/src/handshake.rs` lines 84-95 and 154-166.

The server's `hello` finds that role in `spec.client`, marks the connection with the row's admin flag, and emits `role_presence` carrying the row's `aggregate` annotation. Source: `hello` in `crates/onlyne-server/src/router.rs` lines 274-331. A role outside the spec answers `unauthorized` with field `role`. Source: `crates/onlyne-server/src/router.rs` lines 283-289.

Every envelope that connection sends afterwards takes `from` from the registered role, so the parent ledger records the aggregate role as the sender. Source: `dispatch_client` in `crates/onlyne-server/src/router.rs` lines 75-87.

The adapter protocol defines the same identity as `Mount::Cluster`, whose payload holds `cluster` and `role`. `ClusterMount.role` documents the aggregate role name registered in the parent spec, and the landed client handshake above is the carrier in use. Source: `crates/onlyne-proto/src/adapter.rs` lines 108-126.

The aggregate annotation travels on the same client row as the rest of the role identity, so the parent needs no second registry. `ClientEntry` carries `role`, `key`, and `aggregate` together. Source: `crates/onlyne-config/src/spec.rs` lines 61-88; `crates/onlyne-config/tests/config_contract.rs` lines 40-108.

## Rule 1: an aggregate role is one ordinary `[[client]]` entry

The plan states this rule as:

> aggregate role 在父 spec 中就是一个 `[[client]]` 条目

The parent spec's own example writes that entry as:

```toml
[[client]]
role = "_supervisor"             # 上层看下来的 aggregate role 也写在这里
key = "ed25519/BBBB..."
aggregate = "cluster-b"          # 声明本 role 代表外部 cluster；纯标注，零特殊代码
allowed_senders = ["*"]
allowed_targets = ["_supervisor"]
```

Source: `docs/v1-PLAN.md` §5 lines 245-250.

`aggregate` is a pure annotation. The core delivery path holds no aggregate branch, the router resolves the row through the same `spec.client` lookup it uses for every role, and the parent's ACL decision reads the same fields it reads for every role. Source: `docs/v1-PLAN.md` §5 line 274; `crates/onlyne-server/src/router.rs` lines 274-290.

The child cluster generates this row like any other. `onlyne server generate` walks every `[[client]]` entry, and entries carrying `aggregate` are ordinary local roles inside their own cluster. Source: `docs/v1-PLAN.md` §11 line 383; `crates/onlyne-server/src/generate.rs` lines 113-118.

## Rule 2: `allowed_targets` names only parent-visible roles

The plan states this rule as:

> `allowed_targets` 只含父层可见 role

The parent admits a delivery when the target exists in the parent spec, the target's `allowed_senders` accepts the sender, and the sender's `allowed_targets` names the target. Source: `crates/onlyne-net/src/acl.rs` lines 76-114; `check_acl` in `crates/onlyne-server/src/relay.rs` lines 163-246.

One exception covers the return leg of a dispatch: a `Completion` addressed to the role whose own `Task` row created the task it names is admitted without that pair, because the ledger's dispatch row is the record that the recipient asked for the work. A completion addressed anywhere else still needs the pair. Source: `check_acl` and `task_origin` in `crates/onlyne-server/src/relay.rs`.

A child role name placed in a parent row's `allowed_targets` resolves to nothing in the parent spec, so the parent refuses with `AclDenyReason::UnknownRole`, field `to.role`, and wire code `unknown_role`. Source: `crates/onlyne-net/src/acl.rs` lines 88-92; `crates/onlyne-server/src/relay.rs` lines 175-183.

The server builds that table from the spec rows it loaded, keyed by role name, and rebuilds it on every successful reload. Source: `acl_from_spec` in `crates/onlyne-server/src/state.rs` lines 48-60; `reload` in `crates/onlyne-server/src/router.rs` lines 442-468.

The aggregate row therefore names parent-visible roles in `allowed_targets`, and the supervisor translates a parent delivery into a child delivery on the child side. Source: `docs/v1-PLAN.md` §5 line 274; decision D14 at line 28.

## Rule 3: the child's internal topology stays out of the parent ledger

The plan states this rule as:

> 子层内部拓扑永不出现在父 ledger

Verification case 5 extends it to the child's names inside ledger payloads:

> 父层 ledger 里对 aggregate 的投递只有父层可见，父 ledger 的 `body_json` 不含任何子层 role 名，父层 completion `out_head` 不含子层 role 名。

The parent's durable records are the ledger columns and the event rows, so the rule has a mechanical check:

```sql
ledger(msg_id TEXT PRIMARY KEY, op_id TEXT UNIQUE, fingerprint TEXT, kind TEXT,
       from_json TEXT NOT NULL, to_json TEXT NOT NULL, task TEXT, parent_task TEXT,
       attempt INTEGER NOT NULL, state TEXT NOT NULL, out_head TEXT, reason TEXT,
       enqueued_at TEXT NOT NULL, acked_at TEXT, body_json TEXT)
events(seq INTEGER PRIMARY KEY, type TEXT NOT NULL, data_json TEXT NOT NULL,
       created_at TEXT NOT NULL)
```

Source: `SERVER_DDL` in `crates/onlyne-store/src/server.rs` lines 44-69.

One row is written per accepted send, and its `from_json`, `to_json`, `body_json`, and `out_head` all come from the parent-side envelope. Source: `LedgerRow::from_envelope` in `crates/onlyne-store/src/server.rs` lines 148-170; `relay::send` in `crates/onlyne-server/src/relay.rs` lines 248-345.

The observables a reader sees for that row are rebuilt from the same columns, and the event stream carries the same principals. Source: `entry_from_row` in `crates/onlyne-server/src/relay.rs` lines 596-615; `ledger_event` in `crates/onlyne-server/src/relay.rs` lines 576-593.

A child role name inside `from_json`, `to_json`, `body_json`, `out_head`, or an event `data_json` breaks this rule. Source: `docs/v1-PLAN.md` line 463.

## Cross-cluster addressing

The plan is silent on the address form a parent uses for a child cluster, so this section carries a design pointer beside the landed behaviour. The working design: a cross-cluster address is a `gateway_ref`, and the parent sees the child as a gateway-like conversation. The child's supervisor maps an inbound envelope for the aggregate role onto its own server's `send`. The parent's aggregate row carries only the aggregate role name. to confirm against `crates/onlyne-server/src/relay.rs` (the `resolve_target` match) and `crates/onlyne-testkit/e2e/two-cluster.sh`.

The landed shape today routes a parent delivery to the aggregate role name, because `resolve_target` accepts a role present in `spec.client` and refuses `Principal::Cluster` with `unknown_role` and field `to.cluster`. Source: `crates/onlyne-server/src/relay.rs` lines 90-127. A cluster principal used as a sender requires an admin send. Source: `crates/onlyne-server/src/relay.rs` lines 154-159; `Principal::Cluster` in `crates/onlyne-proto/src/envelope.rs` lines 42-43.

`gateway_ref` is landed as an opaque handle on the host-to-gateway render path, filled from the envelope's `causality.reply_to`, beside `conversation`. Source: `render_outbound` in `crates/onlyne-server/src/gateway_host.rs` lines 119-174; `RenderSendArgs` in `crates/onlyne-proto/src/adapter.rs` lines 316-324. The gateway resolves that handle in its own `gateway_ref(channel, conversation, external_id, scene)` table. Source: `crates/onlyne-gateway/src/refs.rs` lines 63-100; `docs/v1-PLAN.md` S10 line 455.

## Prose exposure

`onlyne cluster export-prose` prints one role's outward description text, and the layer above writes that text into its own `prose`. The command resolves `--role`, defaults to the local role, prints raw prose unless `--json` is set, and issues the existing role query with no new protocol op. Source: `export_prose` in `crates/onlyne-cli/src/admin.rs` lines 501-543; `docs/v1-PLAN.md` S11 line 461.

The client answers that query from its durable prose cache. `prose_cache(role, prose, spec_hash, cached_at)` stores the text with the `spec_hash` it arrived under, so a spec change is detectable. Source: `crates/onlyne-store/src/client.rs` lines 53-58; `docs/v1-PLAN.md` §10 line 368.

The prose itself reaches a client through `welcome`, which carries the role's `prose` and the spec's `spec_hash`, and the generated workspace holds no prose copy. Source: `crates/onlyne-server/src/router.rs` lines 311-330; `docs/v1-PLAN.md` §5 line 272; §11 line 393.

## Limits

The parent cannot see child session projections, because projection rows follow the client connections of the server that holds them, the child's clients connect to the child server, and a projection arrives only through `session_sync` from a client of that server. Source: `sessions` in `SERVER_DDL` (`crates/onlyne-store/src/server.rs` lines 28-43); `ClientOp::SessionSync` in `crates/onlyne-proto/src/ops.rs` line 426; `projection::session_sync` in `crates/onlyne-server/src/router.rs` lines 117-129; decision D5 at `docs/v1-PLAN.md` line 19.

A name collision between a child role and a parent role is the supervisor's responsibility, since the aggregate name is the only identity visible upward. to confirm against `crates/onlyne-net/src/acl.rs` (role names are the ACL key) and `crates/onlyne-testkit/e2e/two-cluster.sh`.

An offline child leaves the parent row `queued`, because the send path marks a row `in_flight` only for a connected role, and the child's own ledger holds nothing about that envelope. Source: `relay::send` in `crates/onlyne-server/src/relay.rs` lines 305-329; `LedgerState::Queued` in `crates/onlyne-proto/src/event.rs` lines 66-80; decision D10 at `docs/v1-PLAN.md` line 24.

## Two-cluster acceptance checklist

Verification case 5 restated as assertions a human can run. Source: `docs/v1-PLAN.md` line 504.

1. The parent server runs with its planner role.
2. The child server runs with its builder role.
3. The parent spec holds one `[[client]]` row whose `role` and `aggregate` are `cluster-b`.
4. The child supervisor's client connects to the parent server with that row's key and presents `role = "cluster-b"` (`hello.args.mount.role` in plan terms).
5. The parent runs `onlyne --server-root <parent-root> send --to cluster-b --text "P1 round trip"`.
6. The child supervisor receives the envelope as the aggregate role and acks it.
7. The parent ledger shows one row with `state = "acked"` and `from.role = "cluster-b"`.
8. The parent ledger shows only aggregate role rows.
9. No child role name and no child prose appears in the parent ledger, `body_json` and `out_head` included.

`crates/onlyne-testkit/e2e/two-cluster.sh` holds these assertions. It stands up a parent cluster with `planner`, a child cluster with `builder`, joins the child supervisor to the parent as `cluster-b`, runs the parent round trip, checks that the parent ledger settles `acked` with `from.role = cluster-b`, and greps every parent ledger `body_json` and `out_head` for the child role name and the child prose. Source: `crates/onlyne-testkit/e2e/two-cluster.sh` lines 54-150.
