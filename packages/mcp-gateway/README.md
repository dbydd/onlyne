# @onlyne/mcp-gateway

An Onlyne node that hosts MCP servers as resources. Each configured server registers as one
resource whose operations are that server's tools, so an agent reaches MCP through the same
resource path it uses for every other capability.

MCP resources carry no platform event stream, so this node has no push ring, no render ring, no
event log, and no handler binding.

## Run

```sh
bunx onlyne-mcp-gateway ./gateway.json
```

```json
{
  "node": { "id": "urn:onlyne:gateway:mcp" },
  "server": { "bind": "127.0.0.1", "port": 3992 },
  "storage": { "path": "./var/mcp/store.json" },
  "servers": [
    {
      "name": "hindsight",
      "url": "http://localhost:8888/mcp/example-bank/",
      "timeout_ms": 60000,
      "controllers": ["urn:onlyne:agent:main"]
    }
  ]
}
```

`headers` adds per-request headers such as an API key and is never logged. `controllers` lists
the nodes allowed to call a server's tools; an empty list leaves the node owner alone.
`language` accepts `en` or `zh-CN` and otherwise follows `ONLYNE_LANG`, `LC_ALL`, or `LANG`.

## Library use

```ts
import { createMcpGateway } from '@onlyne/mcp-gateway';
```

The result carries the node, the connections, and a `fetch` handler.

## License

MIT.
