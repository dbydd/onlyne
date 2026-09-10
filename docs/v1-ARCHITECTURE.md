# Onlyne v1.0.0 Architecture

This onboarding map follows `docs/v1-PLAN.md` and `docs/v1-CONTRACT.md`. Source notes point at the exact plan section or contract section that defines each surface.

## Package graph

The workspace contains fourteen crates plus four gateway plugins during the v1.0.0 cutover. The release graph uses thirteen v1 crates, four plugin crates, and the transient `onlyne-legacy` reference crate named by the contract. Source: `docs/v1-PLAN.md` §1 lines 53-76; `docs/v1-CONTRACT.md` Ownership lines 12-22.

```mermaid
graph TD
  server[onlyne-server] --> proto[onlyne-proto]
  server --> frame[onlyne-frame]
  server --> net[onlyne-net]
  server --> store[onlyne-store]
  server --> config[onlyne-config]
  server --> layout[onlyne-layout]
  client[onlyne-client] --> proto
  client --> frame
  client --> net
  client --> store
  client --> config
  client --> layout
  client --> session[onlyne-session]
  gateway[onlyne-gateway] --> adapter[onlyne-adapter]
  gateway --> proto
  gateway --> frame
  cli[onlyne-cli] --> proto
  cli --> frame
  testkit[onlyne-testkit] --> adapter
  testkit --> proto
  testkit --> session
  store --> session
  net --> proto
  adapter --> proto
  adapter --> frame
  telegram[plugins/onlyne-gateway-telegram] --> adapter
  feishu[plugins/onlyne-gateway-feishu] --> adapter
  qqbot[plugins/onlyne-gateway-qqbot] --> adapter
  weixin[plugins/onlyne-gateway-weixin] --> adapter
  legacy[onlyne-legacy reference] -. ported code source .-> session
```

| Package | Responsibility | Source |
|---|---|---|
| `onlyne-frame` | Length-prefixed JSON codec and stream multiplexing | Plan §1, §4 |
| `onlyne-proto` | Envelope, bodies, message kinds, ops, errors, events, schema export | Plan §1, §3; Contract Cross-crate decisions |
| `onlyne-config` | Server spec, client config, plugin config, TOML schema | Plan §5; Contract Ownership |
| `onlyne-layout` | Server root, workspace discovery, legacy layout refusal | Plan §2 |
| `onlyne-store` | Server ledger DB and client local DB | Plan §10 |
| `onlyne-session` | Lifecycle reducer, `SessionBackend`, reconcile bridge | Plan §6 |
| `onlyne-net` | TLS, cert pinning, ed25519 handshake, ACL, backoff | Plan D9 and S5; Contract line 39 |
| `onlyne-adapter` | Shared adapter SDK and protocol schema | Plan §7 |
| `onlyne-server` | Router, ledger relay, projections, faults, admin, generate | Plan §5, §8, §11, S6, S7 |
| `onlyne-client` | Role runloop, intents, adapter socket, accept/dispatch | Plan §6, §7, S8 |
| `onlyne-gateway` | Platform gateway host and shared rendering/auth kit | Plan S10 |
| `onlyne-cli` | Thin human entrypoint and socket forwarding | Plan §9; Contract CLI vocabulary |
| `onlyne-testkit` | Fake agent, fake gateway, conformance runner, e2e fixtures | Plan S9 and Verification |
| `onlyne-legacy` | Temporary read-only source material before S12 deletion | Contract Repo state; Plan S1, S12 |
| `plugins/onlyne-gateway-telegram` | Telegram gateway plugin crate | Plan §1, S10 |
| `plugins/onlyne-gateway-feishu` | Feishu/Lark gateway plugin crate | Plan §1, S10 |
| `plugins/onlyne-gateway-qqbot` | QQ Bot gateway plugin crate | Plan §1, S10 |
| `plugins/onlyne-gateway-weixin` | Weixin gateway plugin crate | Plan §1, S10 |

## Firewall rules

| Boundary | Rule | Source |
|---|---|---|
| Protocol crate | `onlyne-proto` has zero tokio dependency and owns the public API names. | Plan §1 line 74; Contract line 37-38 |
| Session crate | `onlyne-session` stays pure and exposes `SessionBackend`; store/proto/net stay outside the reducer crate. | Plan §1 line 74; Contract line 40 |
| Server and client | `onlyne-server` and `onlyne-client` share proto, frame, net, store, config, and layout, with no direct dependency between the two binaries. | Plan §1 line 74 |
| Plugins | `plugins/*` depend on `onlyne-adapter` and `onlyne-proto`; server internals stay outside plugin crates. | Plan §1 line 74 |
| Server binary | `onlyne-server` excludes platform SDKs plus `resvg` and `pulldown-cmark`. | Plan §1 line 76 |
| Gateway binary | `onlyne-gateway` excludes ledger, router, and TLS server internals. | Plan §1 line 76 |
| Client binary | `onlyne-client` excludes every platform SDK. | Plan §1 line 76 |
| CLI binary | `onlyne` carries zero business logic and forwards to daemons or sockets. | Plan §9 lines 342-344; Contract lines 61-63 |

## Message model

`Envelope` is the single cross-process message. It carries `protocol`, `id`, `op_id`, `kind`, `from`, `to`, optional `causality`, `body`, `ts`, optional `ttl_ms`, and `admin`. Source: Plan §3 lines 114-175.

| Kind | Meaning | Source |
|---|---|---|
| `Task` | Delivery to a role that creates or reuses a session. | Plan §3 lines 181-182 |
| `Completion` | Terminal receipt for a task, with the result head saved in ledger. | Plan §3 line 183 |
| `Note` | Free text with no session creation and offline rejection. | Plan §3 line 184; Plan line 523 |
| `Control` | `recycle`, `probe`, `snapshot`, or `cancel`, gated by admin flag or task owner. | Plan §3 lines 131-136 and 185 |

| Tier | Members | Delivery semantics | Resync path | Source |
|---|---|---|---|---|
| Control plane | `Task`, `Completion`, `Control` | At-least-once with `op_id` idempotency and durable receipts. | Retry uses the same `op_id`; conflict text is `op_id conflict: request differs from durable receipt`. | Plan D11 lines 25; Plan §3 lines 179-185 |
| Observation plane | `report` and `ev` stream | At-most-once with monotonic `seq`. | Client sends `ping`; `pong.server_seq` beyond `resync_lag` drives `subscribe` with `since_seq`. | Plan D11 line 25; Plan §4 line 214 |

Body validation is a hard constructor rule: text or image must exist, text is capped at 1 MiB, decoded image bytes are capped at 2 MiB, and image mime is one of `image/png`, `image/jpeg`, `image/gif`, or `image/webp`. Source: Plan §3 line 177.

## Frame protocol

Frames use `u32` big-endian length plus UTF-8 JSON. Frames above `MAX_FRAME_BYTES = 8 * 1024 * 1024` produce `error{code:"frame_too_large"}` and close the connection. Source: Plan §4 lines 191-198.

Top-level frame variants share one connection: `req`, `res`, `ev`, `ack`, `ping`, `pong`, and `bye`. Source: Plan §4 lines 199-210.

`res.error.code` is a closed lowercase set: `invalid`, `unknown_op`, `acl_denied`, `unknown_role`, `recipient_offline`, `duplicate`, `conflict`, `unauthorized`, `forbidden`, `not_admin`, `frame_too_large`, `bad_frame`, `protocol_version`, and `internal`. Source: Plan §4 line 212.

## Socket surfaces

| Surface | Endpoint | Security and handshake | Source |
|---|---|---|---|
| Client to server | TCP address from `[server].listen` | TLS 1.3 with rustls, server certificate SPKI pin, preregistered ed25519 role key, ACL before ledger write. | Plan D9 line 23; Plan §5 line 274; Plan S5 line 428 |
| Adapter unix socket | Agent plugins connect to `<workspace>/.onlyne/run/s`; gateway processes connect to `<server-root>/.onlyne/run/s`. | Shared adapter SDK and frame codec; `hello.kind` selects `agent` or `gateway`; mount carries role/session or gateway identity. | Plan §7 lines 291-306 |
| Admin unix socket | `<server-root>/.onlyne/run/s` with mode `0600` | Local trust root; `hello.kind = "admin"`; admin ops use ACL with `--from <role>` on send/control. | Plan §2 lines 83-87; Plan §8 line 320 |

The server listener demultiplexes during `hello`: `agent`, `gateway`, and `admin` select the mounted surface. The `hello` timeout is 5 s. Any pre-hello application frame returns `error{code:"invalid",message:"hello required first"}` and closes the connection. Source: Plan §7 lines 293 and 310.

Socket discovery for CLI commands is fixed: `--socket <path>` wins, `--server-root <dir>` maps to `<dir>/.onlyne/run/s`, and `--workspace <dir>` or upward discovery maps to `.onlyne/run/s`. Failure exits 3 with `onlyne: no onlyne socket found; pass --socket, --server-root, or --workspace`. Source: Plan §9 line 344; Contract lines 65-69.

## Operation vocabulary

| Surface | Closed op set | Source |
|---|---|---|
| Client to server | `hello`, `send`, `pull`, `ack`, `report`, `session_sync`, `subscribe`, `query_ledger`, `query_sessions`, `query_roles`, `query_faults`, `control`, `bye` | Plan §8 line 318 |
| Admin | `status`, `roles`, `sessions`, `ledger`, `faults`, `watch`, `history`, `spec_diff`, `reload`, `send`, `control`, `repair_inspect`, `repair_adopt`, `repair_rebind`, `repair_retry`, `repair_fail`, `repair_close`, `repair_ack` | Plan §8 line 320 |
| Gateway to server | `hello`, `register_channel`, `deliver`, `render_send`, `health`, `typing`, `bye` | Plan §8 line 322 |
| Adapter plugin to host | `hello`, `welcome`, `report`, `session_register`, `assign_ack`, `send`, `deliver`, `detach` | Plan §7 lines 295-306 |
| Host to plugin | `welcome`, `assign`, `render_send`, `probe`, `recycle`, `config_get`, `bye` | Plan §7 line 308 |

The old vocabulary is gone: `loopback`, `swarm_ready`, `swarm_recycled`, `swarm_busy`, `swarm_idle`, `mark_io_consumed`, `consume`, `start_adapter`, `stop_adapter`, `restart_adapter`, the old `fetch_history_page` shape, and raw `onlyne client '<json>'` pass-through. Source: Plan §8 line 324.

## Ledger state

The server ledger table stores `queued`, `in_flight`, `acked`, `rejected`, and `expired`. Body JSON is retained through ack and later pruned by retention. Source: Plan §10 lines 357-360.

| Current state | Legal next states | Entry and exit meaning | Source |
|---|---|---|---|
| start | `queued` | Accepted `send` writes the durable row after ACL. | Plan §5 line 274; Plan S6 line 432 |
| `queued` | `in_flight`, `expired`, `rejected` | Pull delivery moves work to a live role; TTL expiry creates `expired`; hard route or operator failure can create `rejected`. | Plan S6 line 432; Plan §10 line 360; inference from admin repair verbs in Plan §8 line 320 |
| `in_flight` | `acked`, `rejected` | `ack` settles successful delivery; operator repair can fail the row. | Plan S6 line 432; inference from `repair_fail` in Plan §8 line 320 |
| `acked` | terminal | Completed delivery with receipt and optional `out_head`. | Plan S6 line 432; Plan Verification case 1 lines 496-498 |
| `rejected` | terminal | Durable refusal after the operation has a row. | Plan §10 line 360; inference from closed error set in Plan §4 line 212 |
| `expired` | terminal | TTL-driven terminal state. | Plan §10 line 360; Plan Verification case 6 line 505 |

Idempotency is keyed by `op_id`. A repeated identical request returns the durable receipt with `duplicate`; a different request with the same `op_id` returns `conflict` and `op_id conflict: request differs from durable receipt`. Source: Plan §3 line 179; Verification case 3 line 502.

## Session lifecycle

The client owns execution state authority. The server stores projections. Source: Plan D5 line 19; Plan §6 lines 276-289.

| Dimension | Values | Source |
|---|---|---|
| `AgentState` | `Booting`, `Ready`, `Running`, `Idle`, `Gone` | Plan §6 line 280 |
| `DeliveryState` | `None`, `Pending`, `Retrying`, `Accepted`, `Exhausted` | Plan §6 line 280 |
| `ResourceState` | `Detached`, `Attached`, `Closing`, `Closed` | Plan §6 line 280 |
| `RecoveryState` | `None`, `IdleWaiting`, `IdleFault`, `Draining` | Plan §6 line 280 |
| `Outcome` | `Done`, `Failed`, `Cancelled` | Plan §3 line 140 |
| `PublicLifecycle` | `Created`, `Working`, `Idle`, `Exited` | Plan §6 line 280 |

The client task path is mechanical: apply reuse policy, reuse an idle session or spawn `SessionBackend`, record `sessions`, report `ready`, then deliver the assignment. Source: Plan §6 line 285.

Disconnect behavior is mechanical: stop accepting new delivery, let running sessions reach terminal state, persist completion intents, reconnect with capped backoff, then flush intents in `seq` order. Source: Plan §6 lines 287-289.

Deletion list from the lifecycle port:

- Automatic recovery task generation is deleted.
- Dead-terminal sweep re-delivery is deleted.
- `hop_timeouts` replay trigger is deleted.
- `events.rs::replay_ready_history` is deleted.
- `MAX_ATTEMPTS` automatic retry path is deleted.
- `DeliveryState::Exhausted` stays terminal.

Source: Plan §6 line 282; Plan S3 line 420; Plan line 527.

## Workspace generation

`spec.toml` is protocol truth: role name, public key, ACL, prose, concurrency, timeout, and `session_command`. Template directories hold workspace content. Source: Plan §11 lines 372-375.

Generation command:

```bash
onlyne server generate --root <server-root> [--template <relative-path>]... [--role <name>]... [--out <dir>] [--force]
```

Source: Plan §11 lines 376-381.

Generation flow:

1. Read `<server-root>/.onlyne/spec.toml`.
2. Select all `[[client]]` rows or the intersection of `--template` and `--role` filters.
3. Map each role to a template directory whose basename equals the role name.
4. Mirror the template topology into `<out>/<template-relative-path>/<role>/`.
5. Create `.onlyne/config.toml` and `.onlyne/keys/role.key` with a new ed25519 keypair.
6. Copy template content and merge a template `.onlyne/config.toml` fragment with derived values taking priority.
7. Replace the closed placeholder set: `{{role}}`, `{{cluster}}`, `{{server_name}}`, `{{listen}}`, `{{cert_pin}}`, `{{admin}}`, `{{max_sessions}}`, `{{agent_package}}`.
8. Vendor `[server].agent_package` into `<ws>/.onlyne/agent/<pkg-name>/` when the template uses `{{agent_package}}`.
9. Scan output bytes for the generated output root and server root absolute prefixes.
10. Delete this generation output on absolute-path match and return `onlyne: generated workspace embeds absolute path <path>`.
11. Print a `[[client]]` TOML fragment and `<out>/.onlyne-generation.json`.
12. Leave `spec.toml` editing to the operator or supervisor, followed by `onlyne server reload`.

Source: Plan §11 lines 383-397.

Generation errors with exit 4 include `onlyne: no role matches the requested templates/roles`, `onlyne: template for role <r> is ambiguous: <p1>, <p2>`, `onlyne: no template directory named <r> under <template_root>`, `onlyne: refusing to overwrite <path>; pass --force`, `onlyne: agent_package not set in spec.toml [server]`, and `onlyne: generated workspace embeds absolute path <path>`. Source: Plan §11 lines 383-391.

Relocation guarantee: generated workspaces derive runtime paths from their own `--workspace`, and server connection uses `listen` plus `cert_pin`. Source: Plan §11 line 391; Verification case 9 lines 509-518.

## Federation

Federation uses ordinary role rows. A child cluster exposes one aggregate role to its parent server, and that aggregate role is a normal `[[client]]` entry with its own key. Source: Plan D14 line 28; Plan §5 lines 245-249; Plan S11 lines 458-463.

The parent ledger sees aggregate role traffic. Child role names and child prose stay below the aggregate boundary. Source: Verification case 5 line 504; Plan S11 line 463.

`onlyne cluster export-prose` prints the outward prose for the aggregate role and adds no protocol op. Source: Plan S11 line 461; Contract line 63.

## Where the code lives

| Responsibility | Crate or path | Spec origin |
|---|---|---|
| Frame encode/decode | `crates/onlyne-frame/` | Plan §4 |
| Wire types and schemas | `crates/onlyne-proto/` | Plan §3; Contract line 38 |
| Server TOML and client config | `crates/onlyne-config/` | Plan §5 |
| Layout and legacy refusal | `crates/onlyne-layout/` | Plan §2 |
| Ledger and local databases | `crates/onlyne-store/` | Plan §10 |
| Lifecycle reducer and backend trait | `crates/onlyne-session/` | Plan §6 |
| TLS, ed25519, ACL, backoff | `crates/onlyne-net/` | Plan D9; Plan S5; Contract line 39 |
| Adapter SDK | `crates/onlyne-adapter/` | Plan §7 |
| Conformance and fake binaries | `crates/onlyne-testkit/` | Plan S9; Verification |
| Server runtime | `crates/onlyne-server/` | Plan §5, §8, §11, S6, S7 |
| Client runtime | `crates/onlyne-client/` | Plan §6, §7, S8 |
| Gateway runtime | `crates/onlyne-gateway/` | Plan §7, §8, S10 |
| Human CLI | `crates/onlyne-cli/` | Plan §9; Contract CLI vocabulary |
| Telegram plugin | `plugins/onlyne-gateway-telegram/` | Plan S10 |
| Feishu plugin | `plugins/onlyne-gateway-feishu/` | Plan S10 |
| QQ Bot plugin | `plugins/onlyne-gateway-qqbot/` | Plan S10 |
| Weixin plugin | `plugins/onlyne-gateway-weixin/` | Plan S10 |
| Temporary legacy reference | `crates/onlyne-legacy/` | Contract Repo state; Plan S1/S12 |
