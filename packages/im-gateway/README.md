# @onlyne/im-gateway

An Onlyne node that hosts IM conversations as resources. Every platform conversation becomes one
resource with its own binding and event log, and the bound handler receives each event as a
push. The gateway keeps a thread per conversation so an agent can answer through the same
`msg` path it uses for every other message.

The process also publishes itself over DNS-SD and draws a trust terminal: peer list, exchange
stage, fingerprints, and the four actions that change one relation.

## Run

```sh
bunx onlyne-im-gateway ./gateway.json
```

```json
{
  "node": { "id": "urn:onlyne:gateway:im" },
  "server": { "bind": "127.0.0.1", "port": 3991 },
  "storage": { "path": "./var/im/store.json" },
  "credentials_dir": "./credentials",
  "media_dir": "./var/im/media",
  "connectors": [
    { "platform": "lark", "assignee": "urn:onlyne:agent:main", "credential_file": "feishu" }
  ]
}
```

`ONLYNE_NODE_ID`, `ONLYNE_BIND`, and `ONLYNE_PORT` override the file for one host.
`ONLYNE_NO_TUI=1` keeps the JSON log on stdout instead of the trust terminal. `language` accepts
`en` or `zh-CN` and otherwise follows `ONLYNE_LANG`, `LC_ALL`, or `LANG`.

Install the connector package for each platform you serve, for example `@onlyne/im-lark`.

## Library use

```ts
import { createImGateway } from '@onlyne/im-gateway';
```

`createImGateway` returns the node, the connectors, the push ring, and a `fetch` handler. A host
that embeds the gateway supplies its own listener.

## License

MIT.
