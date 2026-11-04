# @onlyne/mcp-server

An [MCP](https://modelcontextprotocol.io) server that exposes one Onlyne node's core
capabilities as tools and resources. It is the reverse of `@onlyne/mcp-gateway`:

| Package | Direction |
|---|---|
| `@onlyne/mcp-gateway` | Onlyne node that hosts **remote MCP servers** as resources |
| `@onlyne/mcp-server` | MCP server that hosts **this Onlyne node** as tools |

The process is a real Onlyne node (identity, HTTP `/onlyne/v3`, discovery, trust). MCP is an
additional surface. The required MCP transport is stdio, so an MCP client can spawn the binary.

## Run

```sh
bunx onlyne-mcp-server ./mcp-server.json
```

```json
{
  "node": { "id": "urn:onlyne:agent:mcp" },
  "server": { "bind": "127.0.0.1", "port": 3996 },
  "storage": { "path": "./var/mcp-server/store.json" }
}
```

`ONLYNE_NODE_ID`, `ONLYNE_BIND`, and `ONLYNE_PORT` override the file. Logs go to stderr and to
`mcp-server.log` beside the store — stdout is reserved for MCP.

The CLI always creates or loads an identity key under `~/.config/onlyne/keys/`. Pair this node
with peers the same way as any other Onlyne node (trust terminal on a gateway, `/onlyne` on OMP).
This process skips the trust TUI so it cannot corrupt the stdio stream.

This is a **separate identity from `@onlyne/grok-bot`**. Grok Bot fan-out on one account is
isolated by grok-bot threads (`grokbot:dm:<agent_id>`, `grokbot:group:<group_id>`) under the
single grok-bot node / account key. MCP tools read **this** node's store — inbox, threads,
neighbors — not the grok-bot node's, unless the two identities are later unified. Running
MCP as its own process is the supported split.

## Register in an MCP client

Cursor / Claude Desktop / any stdio MCP client:

```json
{
  "mcpServers": {
    "onlyne": {
      "command": "onlyne-mcp-server",
      "args": ["/absolute/path/to/mcp-server.json"]
    }
  }
}
```

With Bun from this repository:

```json
{
  "mcpServers": {
    "onlyne": {
      "command": "bun",
      "args": ["packages/mcp-server/src/main.ts", "examples/cluster/gateways/mcp-server.json"]
    }
  }
}
```

The client owns the process. Stdio is the MCP session; the node still binds the configured HTTP
port so cluster peers can reach it.

## Tools

| Tool | What it does |
|---|---|
| `send_message` | Send from this node (`to`, `msg`, optional thread / reply / plan) |
| `list_inbox` | Local inbox |
| `read_message` | One stored message by `sender` + `id` |
| `list_threads` | Threads this node owns |
| `list_joined` | Threads this node was invited into |
| `describe_thread` | Thread + members |
| `list_messages` | Messages in a thread (`attention`: `inbox` or `thread`) |
| `list_neighbors` | Trust-table rows |
| `list_resources` | Resources this node owns |
| `describe_resource` | One resource |
| `node_status` | Node id, inbox size, thread count, neighbor count |

`send_message` uses `response: none` and `execution: existing` unless you pass `async` / `new`.
It does not choose a plan itself. `list_threads` / `list_joined` / `list_resources` /
`list_messages` / `list_inbox` accept `page_size` and `cursor`. `describe_thread` reads a
thread this node owns, or the owner's record for a thread this node was invited into.

Handover, resource operations against a remote gateway, and SystemOne `msg()` are left to OMP
and the existing HTTP/resource path. This server is the read/send surface an MCP client needs
to participate in a cluster.

## Resources

| URI | Contents |
|---|---|
| `onlyne://status` | Identity and counts |
| `onlyne://inbox` | Inbox snapshot |
| `onlyne://threads` | Owned threads |
| `onlyne://neighbors` | Trust table |

## Library use

```ts
import { createOnlyneHost, createOnlyneMcpServer } from '@onlyne/mcp-server';
import { InMemoryTransport } from '@modelcontextprotocol/sdk/inMemory.js';

const host = await createOnlyneHost(config, { storage: 'memory' });
const server = createOnlyneMcpServer(host.node);
```

## License

MIT.
