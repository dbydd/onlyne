# @onlyne/im-gateway

One Onlyne node serves one IM channel. Every platform conversation becomes one resource with its
own binding and event log, and the bound handler receives each event as a push. The gateway keeps
a thread per conversation so an agent can answer through the same `msg` path it uses for every
other message.

The process also publishes itself over DNS-SD and draws a trust terminal: peer list, exchange
stage, fingerprints, and the four actions that change one relation.

Run one process per platform. Lark, QQ, and WeChat use separate node ids, ports, stores, identity
keys, and trust tables. Their connection models differ: Lark holds a WebSocket, QQ holds one
WebSocket per shard on the platform gateway (or receives the signed callback, see
`channel.transport`), and WeChat long-polls the platform.

## Run

```sh
bunx onlyne-im-gateway ./gateway.json
```

```json
{
  "node": { "id": "urn:onlyne:gateway:im-lark" },
  "server": { "bind": "127.0.0.1", "port": 3991 },
  "storage": { "path": "./var/im-lark/store.json" },
  "credentials_dir": "./credentials",
  "media_dir": "./var/im-lark/media",
  "channel": { "platform": "lark", "assignee": "urn:onlyne:agent:main", "credential_file": "feishu" }
}
```

The node id is derived from the channel platform: `urn:onlyne:gateway:im-<platform>`. A second
instance of the same platform may add a suffix, as in `urn:onlyne:gateway:im-qq-test`.
`ONLYNE_NODE_ID`, `ONLYNE_BIND`, and `ONLYNE_PORT` override the file for one host.
`ONLYNE_NO_TUI=1` keeps the JSON log on stdout instead of the trust terminal. `language` accepts
`en` or `zh-CN` and otherwise follows `ONLYNE_LANG`, `LC_ALL`, or `LANG`.

`connectors` is refused. Split a file with several connectors into one file per platform. The
`channel.transport` setting names the receive path. It defaults to `websocket` on QQ — the
platform's own gateway, which needs no public endpoint — and `webhook` selects the signed callback
instead; any other platform has a single transport and refuses `websocket`. `channel.webhook_path`
defaults to `/im/webhook` and addresses the callback only: with the gateway socket selected no
path is routed to the connector, and the connector's status reports the transport it runs.

The node publishes an execution context directory at `/onlyne/v3/contexts`. `contexts` names the
aliases it lists and defaults to `["main"]` — the alias a sender's send resolves when it continues
work on this gateway without naming a context, so a drafted reply needs no explicit
`meta.context`. An explicit empty array publishes an empty directory. The gateway consumes no
model context: the aliases are delivery labels, and a reply lands in the conversation's thread.

Credentials resolve in this order: `channel.credential`, `channel.credential_file`, then the
platform variables. Lark uses `FEISHU_APP_ID` with `FEISHU_APP_SECRET`. QQ uses
`QQ_BOT_APP_ID` with `QQ_BOT_APP_SECRET`. WeChat uses `WECHAT_ILINK_BOT_TOKEN`. A missing
credential does not stop startup: the connector reports onboarding with the fields still needed,
and the trust terminal shows that state.

Install the connector package for the platform you serve: `@onlyne/im-lark`, `@onlyne/im-qq`, or
`@onlyne/im-wechat`.

## Diagnosis

Read live state from the gateway's own listener, never from a startup log line:

```sh
curl http://127.0.0.1:3991/im/status
```

The answer is JSON: the node, platform and transport; the connector's state and counters; every
conversation resource with its handler, cursor and revision; and how many pushes are still owed.
`transport` is the receive path the connector reports, then the configured `channel.transport`,
then `platform` for a platform with a single built-in path.
`/im/status` is a local host surface — the `/onlyne/v3` protocol routes are untouched. The trust
terminal (`onlyne-im-gateway` on a TTY) renders the same view through `ImGateway.statusLines()`,
which a host without a terminal can call directly. `ImGateway.statusReport()` returns the JSON.

`im.gateway.listening` is written once at startup with static fields only — `node`, `endpoint`,
`platform`, `transport`, `storage`. It is not a state snapshot: it never names the connector's
state, the counters or a binding.

## Log levels

The host logger writes JSON lines. `ONLYNE_LOG_LEVEL` selects the threshold (`debug`, `info`,
`warn`, `error`); the default is `info`, and the ring the TUI renders plus the file sink keep
every line that gets through.

| Level | Meaning | Examples |
|---|---|---|
| `debug` | Per-pass and protocol internals, off by default | `inbound.acked`, a reconcile pull, a replayed todo |
| `info` | State transitions and business milestones, one line each | `im.gateway.listening`, `connector.state`, `inbound.event`, `resource.binding`, `resource.push` accepted, `platform.send` committed, `gateway.maintain.summary` |
| `warn` | Degraded but running | `connector.state` to an error, `resource.push` refused or timed out, `resource.push` with no thread or handler, `platform.send` refused, `im.render.ambiguous`, `reconcile.failed`, a channel that could not start |
| `error` | The operation failed and the caller lost something | `im.receive.failed` |

Message bodies never appear in a log line: `len` and `sha8` (the first eight hex digits of the
SHA-256 of the UTF-8 body) identify a body instead.

## Library use

```ts
import { createImGateway } from '@onlyne/im-gateway';
```

`createImGateway` returns the node, the connector, the push ring, a `fetch` handler, a `webhook`
handler, `maintain()`, `status()`, `statusReport()`, and `statusLines()`. A host that embeds the
gateway supplies its own listener, routes the platform callback path to `webhook`, and may route
`/im/status` to `statusReport()`.

## Migrating an aggregated store

An aggregated `urn:onlyne:gateway:im` store cannot keep its owner references under a new node id.
Use the offline migration command to plan the move before changing anything:

```sh
bun packages/im-gateway/src/migrate-main.ts plan ./var/im/store.json --platform lark
```

The command reports every resource, thread, conversation, and binding that moves, and the key
directory the new identity needs. `apply` writes a snapshot beside the old store before it
rewrites owner references, and `rollback` restores that snapshot. Migration is offline: stop the
old gateway first. Remote peers hold the old owner references, so they must discover and pair
with the new identity after the move. A signed handover cannot rewrite their stored references.

## License

MIT.
