import { OnlyneNode, type Auth, type Json, type Plugins } from '@onlyne/lib';
import { MemoryStorage } from '@onlyne/lib/memory';
import { createMcpBridge, McpConnection, resourceOf, type McpServerOptions } from '../src/index.js';

let failures = 0;
const ok = (label: string, condition: boolean): void => {
  if (condition) console.log(`ok   ${label}`);
  else { failures += 1; console.error(`FAIL ${label}`); }
};

/**
 * A scripted MCP server: one Bun.serve speaking the HTTP streamable transport. It hands out
 * session ids, answers `tools/list` from a fixed set, and routes `tools/call` to fixed handlers
 * — the same role `im-fake` plays for the IM gateway. The transport surface is the official
 * SDK's, so the connection layer is exercised exactly as it talks to a real server.
 */
class FakeMcpServer {
  readonly url: string;
  readonly tools = [
    { name: 'echo', description: 'return the input', inputSchema: { type: 'object' }, annotations: { readOnlyHint: true } },
    { name: 'boom', description: 'always fails', inputSchema: { type: 'object' }, annotations: { readOnlyHint: true } },
    { name: 'write', description: 'side effect', inputSchema: { type: 'object' } },
  ];
  readonly calls: Array<{ name: string; arguments: Json }> = [];
  #server: { stop: () => Promise<void> };

  constructor() {
    const handler = async (request: Request): Promise<Response> => {
      if (request.method === 'GET') {
        // The SDK opens a listening GET stream; the fake has nothing to push, so it idles.
        return new Response(null, { status: 405 });
      }
      const body = JSON.parse(await request.text()) as { method: string; id?: string | number; params?: unknown };
      const respond = (payload: unknown, sessionId?: string): Response => new Response(
        `event: message\ndata: ${JSON.stringify(payload)}\n\n`,
        { headers: { 'content-type': 'text/event-stream', ...(sessionId === undefined ? {} : { 'mcp-session-id': sessionId }) } },
      );
      if (body.method === 'initialize') {
        const sessionId = crypto.randomUUID();
        return respond({ jsonrpc: '2.0', id: body.id, result: { protocolVersion: '2025-03-26', capabilities: { tools: {} }, serverInfo: { name: 'fake', version: '0' } } }, sessionId);
      }
      if (body.method === 'notifications/initialized') return new Response(null, { status: 202 });
      if (body.method === 'ping') return respond({ jsonrpc: '2.0', id: body.id, result: {} });
      if (body.method === 'tools/list') {
        return respond({ jsonrpc: '2.0', id: body.id, result: { tools: this.tools } });
      }
      if (body.method === 'tools/call') {
        const params = body.params as { name: string; arguments?: Json };
        this.calls.push({ name: params.name, arguments: params.arguments ?? {} });
        if (params.name === 'boom') {
          return respond({ jsonrpc: '2.0', id: body.id, result: { content: [{ type: 'text', text: 'exploded' }], isError: true } });
        }
        if (params.name === 'write') {
          return respond({ jsonrpc: '2.0', id: body.id, result: { content: [{ type: 'text', text: 'written' }], structuredContent: { done: true } } });
        }
        return respond({ jsonrpc: '2.0', id: body.id, result: { content: [{ type: 'text', text: JSON.stringify(params.arguments) }], structuredContent: params.arguments as object } });
      }
      return respond({ jsonrpc: '2.0', id: body.id, error: { code: -32601, message: 'unknown method' } });
    };
    const server = Bun.serve({ port: 0, hostname: '127.0.0.1', fetch: handler });
    this.#server = server;
    this.url = `http://127.0.0.1:${server.port}/mcp`;
  }

  stop(): Promise<void> { return this.#server.stop(); }
}

const fake = new FakeMcpServer();
const NODE = 'urn:onlyne:gateway:mcp';
const CALLER = 'urn:onlyne:agent:lazuli';

const server: McpServerOptions = { name: 'fake', url: fake.url, timeoutMs: 5_000 };
const connection = new McpConnection(server);
await connection.start();
ok('the connection lists the scripted tools', connection.tools.length === 3 && connection.tools[0]?.name === 'echo');

const definition = resourceOf(NODE, server, connection.tools, [CALLER]);
ok('the resource maps the tool names to operations', definition.operations.length === 3
  && definition.operations.includes('echo') && definition.resource_ref.id === 'fake'
  && (definition.data as { url?: string }).url === fake.url);

const bridge = createMcpBridge([{ server, connection }]);
const plugins: Omit<Plugins, 'storage' | 'resources'> = {
  authenticate: async () => null,
  resolve: async () => { throw new Error('unreached'); },
  decide: async () => { throw new Error('unreached'); },
  runtime: { notify: () => undefined, supports: async () => false, inject: async () => { throw new Error('unreached'); } },
};
const node = new OnlyneNode({
  node_id: NODE,
  base_url: 'http://127.0.0.1:3996/onlyne/v3',
  plugins: { ...plugins, storage: new MemoryStorage(), resources: bridge },
  now: () => Date.now(),
  id: () => crypto.randomUUID(),
});

const ref = definition.resource_ref;
await node.resources.register({ actor: NODE } satisfies Auth, {
  resource_ref: ref,
  kind: 'mcp',
  operations: definition.operations,
  // RFC 0006 §3: the operating agent is the handler; the endpoint keeps read access.
  binding: { handler: CALLER, cursor: '0' },
  log_readers: [NODE],
  event_seq: '0',
  revision: 'rev-1',
  retention_ms: 60_000,
  data: definition.data,
});

const caller: Auth = { actor: CALLER };
const mutation = () => ({
  idempotency_key: crypto.randomUUID(),
  retry_until: new Date(Date.now() + 60_000).toISOString(),
});

const echoed = await node.resources.operate(caller, ref, {
  operation_id: 'op-1', operation: 'echo', data: { hello: 'world' }, ...mutation(),
});
const echoStructured = (echoed.data as { structuredContent?: { hello?: string } } | undefined)?.structuredContent;
ok('a tool call commits with the structured content', echoed.state === 'committed' && echoStructured?.hello === 'world');
ok('the arguments reached the server as tools/call', fake.calls[0]?.name === 'echo' && (fake.calls[0]?.arguments as { hello?: string })?.hello === 'world');

const refused = await node.resources.operate(caller, ref, {
  operation_id: 'op-2', operation: 'boom', ...mutation(),
});
ok('a read-only tool failure settles as aborted with the source text', refused.state === 'aborted'
  && (refused.error?.details as { source?: string } | undefined)?.source === 'exploded');

const sideEffect = await node.resources.operate(caller, ref, {
  operation_id: 'op-3', operation: 'write', data: { x: 1 }, ...mutation(),
});
ok('a side-effecting tool commits on success', sideEffect.state === 'committed');

let unknownTool = false;
try {
  await node.resources.operate(caller, ref, {
    operation_id: 'op-4', operation: 'not_a_tool', ...mutation(),
  });
} catch (error) {
  unknownTool = (error as { error?: { code?: string } }).error?.code === 'unsupported_delivery';
}
ok('a tool outside tools/list never leaves the node', unknownTool && fake.calls.length === 3);

let badArguments = false;
try {
  await node.resources.operate(caller, ref, {
    operation_id: 'op-5', operation: 'echo', data: 'not-an-object', ...mutation(),
  });
} catch (error) {
  badArguments = (error as { error?: { code?: string } }).error?.code === 'invalid_request';
}
ok('non-object arguments are rejected before the server', badArguments && fake.calls.length === 3);

// An unreachable server presents the tools it was configured with, so the call is attempted
// and the transport failure settles as `unknown`.
const deadServer = { name: 'dead', url: 'http://127.0.0.1:1/mcp' };
const dead = new McpConnection({ ...deadServer, timeoutMs: 500 });
dead.tools.push({ name: 'x' });
const deadBridge = createMcpBridge([{ server: deadServer, connection: dead }]);
const deadOutcome = await deadBridge.execute(
  { resource_ref: { owner: NODE, id: 'dead' }, kind: 'mcp', operations: ['x'], revision: 'r', binding: { handler: NODE, cursor: '0' }, log_readers: [NODE], event_seq: '0', retention_ms: 0 },
  { operation_id: 'op-dead', operation: 'x', ...mutation() } as never,
);
ok('an unreachable server is unknown, not aborted', deadOutcome.state === 'unknown');

const reEchoed = await node.resources.operate(caller, ref, {
  operation_id: 'op-6', operation: 'echo', data: { again: true }, ...mutation(),
});
ok('calls keep working across operations', reEchoed.state === 'committed'
  && (fake.calls.at(-1)?.arguments as { again?: boolean })?.again === true);

await connection.close();
await fake.stop();

if (failures > 0) {
  console.error(`${failures} failure(s)`);
  process.exit(1);
}
console.log('mcp-core verify: all checks passed');
