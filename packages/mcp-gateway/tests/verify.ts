import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createMcpGateway, type GatewayConfig, type McpGateway } from '../src/index.js';
import { FakeMcpServer } from './fake-mcp.js';

let failures = 0;
const ok = (label: string, condition: boolean): void => {
  if (condition) console.log(`ok - ${label}`);
  else { failures++; console.error(`not ok - ${label}`); }
};

const fake = new FakeMcpServer();
const NODE = 'urn:onlyne:gateway:mcp';
const CALLER = 'urn:onlyne:agent:tester';

const dir = await mkdtemp(join(tmpdir(), 'mcp-gateway-'));
const config: GatewayConfig = {
  node: { id: NODE },
  server: { bind: '127.0.0.1', port: 3998 },
  storage_path: join(dir, 'store.json'),
  servers: [
    { name: 'fake', url: fake.url, controllers: [CALLER] },
  ],
};

const gateway: McpGateway = await createMcpGateway(config);
const endpoint = 'http://127.0.0.1:3998/onlyne/v3';

const call = async (method: string, path: string, body?: unknown, actor: string = CALLER): Promise<{ status: number; body: Record<string, unknown> }> => {
  const response = await gateway.node.fetch(new Request(endpoint + path, {
    method,
    headers: { 'content-type': 'application/json', 'Onlyne-Actor': actor },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  }));
  const text = await response.text();
  return { status: response.status, body: (text === '' ? {} : JSON.parse(text)) as Record<string, unknown> };
};

const mutation = () => ({
  idempotency_key: crypto.randomUUID(),
  retry_until: new Date(Date.now() + 60_000).toISOString(),
});

// 1. The MCP resource registered with tools mapped to operations.
const resource = await call('GET', `/resources/${NODE}/fake`);
const ops = resource.body.operations as string[] | undefined;
ok('the mcp server registered as an Onlyne resource', resource.status === 200
  && ops?.includes('echo') === true && ops?.includes('write_data') === true);

// 2. A tool call commits through the gateway.
const echoed = await call('POST', `/resources/${NODE}/fake/operations`, {
  ...mutation(), operation_id: 'op-1', operation: 'echo', data: { val: 42 },
});
const structured = (echoed.body.data as { structuredContent?: { val?: number } } | undefined)?.structuredContent;
ok('tools/call commits through the resource operation endpoint', echoed.status === 201
  && echoed.body.state === 'committed' && structured?.val === 42);

// 3. Operation outside tools/list is refused locally.
const refused = await call('POST', `/resources/${NODE}/fake/operations`, {
  ...mutation(), operation_id: 'op-2', operation: 'nonexistent', data: {},
});
ok('an undeclared tool is refused without calling upstream', refused.status === 422
  && (refused.body.error as { code?: string } | undefined)?.code === 'unsupported_delivery'
  && fake.calls.length === 1);

// 4. Multiple servers can be hosted on one gateway node.
await gateway.stop();
const fake2 = new FakeMcpServer();
const multiConfig: GatewayConfig = {
  ...config,
  servers: [
    { name: 'fake1', url: fake.url, controllers: [CALLER] },
    { name: 'fake2', url: fake2.url, controllers: [CALLER] },
  ],
};
const multiGateway: McpGateway = await createMcpGateway(multiConfig);
const r1 = await multiGateway.node.resources.describe({ actor: NODE }, { owner: NODE, id: 'fake1' });
const r2 = await multiGateway.node.resources.describe({ actor: NODE }, { owner: NODE, id: 'fake2' });
ok('one gateway hosts multiple MCP servers as distinct resources',
  r1.operations.includes('echo') && r2.operations.includes('echo') && r1.resource_ref.id === 'fake1' && r2.resource_ref.id === 'fake2');

await multiGateway.stop();
await fake.stop();
await fake2.stop();
await rm(dir, { recursive: true, force: true });

if (failures > 0) {
  console.error(`mcp-gateway verify: ${failures} check(s) failed`);
  process.exit(1);
}
console.log('mcp-gateway verify: all checks passed');
