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

Credentials resolve in this order: `channel.credential`, `channel.credential_file`, then the
platform variables. Lark uses `FEISHU_APP_ID` with `FEISHU_APP_SECRET`. QQ uses
`QQ_BOT_APP_ID` with `QQ_BOT_APP_SECRET`. WeChat uses `WECHAT_ILINK_BOT_TOKEN`. A missing
credential does not stop startup: the connector reports onboarding with the fields still needed,
and the trust terminal shows that state.

Install the connector package for the platform you serve: `@onlyne/im-lark`, `@onlyne/im-qq`, or
`@onlyne/im-wechat`.

## Status

The terminal's status area shows the channel, its connection state, event and send counts,
reconnect count, rate-limit hits, the time of the last event, onboarding fields, and the last
error. `ImGateway.statusLines()` returns the same lines for a host without a terminal.

## Library use

```ts
import { createImGateway } from '@onlyne/im-gateway';
```

`createImGateway` returns the node, the connector, the push ring, a `fetch` handler, a `webhook`
handler, `maintain()`, and `statusLines()`. A host that embeds the gateway supplies its own
listener, and routes the platform callback path to `webhook`.

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
