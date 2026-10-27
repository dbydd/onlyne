# @onlyne/mcp-core

An MCP client session and the `ResourceDriver` that maps MCP onto Onlyne resource operations.

One MCP server is one Onlyne resource. Its `tools/list` names become the resource's `operations`,
and `tools/call` becomes the operation call. The official MCP SDK owns the session id, the
handshake, SSE parsing, and reconnect semantics.

## Operation outcomes

| MCP result | Onlyne outcome |
|---|---|
| Normal result | `committed`, with `content` and `structuredContent` |
| `isError: true` | `aborted` for a read-only tool, `unknown` for one with side effects, with the source text in `details` |
| JSON-RPC protocol error | `invalid_request`; the operation id is never claimed |
| Transport failure or timeout | `unknown`, with the source text |

A tool outside the cached list never leaves the node.

## Example

```ts
import { McpConnection } from '@onlyne/mcp-core';

const connection = new McpConnection({ name: 'hindsight', url: 'http://localhost:8888/mcp/example-bank/' });
await connection.start();
const outcome = await connection.call('recall', { query: 'protocol notes' });
```

## Runtime

The package ships TypeScript source for Bun.

## License

MIT.
