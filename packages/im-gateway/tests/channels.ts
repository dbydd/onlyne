import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import type { Server } from 'bun';
import type { ImEvent } from '@onlyne/im-core';
import { createImGateway, type GatewayConfig, type ImGateway } from '../src/index.js';

/**
 * Three independent channel nodes of the same platform: distinct identities, ports and stores.
 * The point is isolation — an event accepted by one node must leave no row in another's store,
 * and no node may hold another node's identity or trust material.
 */

let failures = 0;
const ok = (label: string, condition: boolean): void => {
  if (condition) console.log(`ok - ${label}`);
  else { failures++; console.error(`not ok - ${label}`); }
};
const settle = async (): Promise<void> => {
  const { promise, resolve } = Promise.withResolvers<void>();
  setTimeout(resolve, 20);
  await promise;
};
const event = (source: string, conversation: string): ImEvent => ({
  platform: 'fake', source_id: source, conversation, kind: 'message',
  from: 'ou_sender', text: `消息 ${source}`, attachments: [], message_handle: `fk_${source}`,
  at: new Date().toISOString(),
});

const root = await mkdtemp(join(tmpdir(), 'im-channels-'));
const nodes: Array<{ gateway: ImGateway; config: GatewayConfig; server: Server<undefined>; resource: string }> = [];
try {
  for (const suffix of ['a', 'b', 'c'] as const) {
    const directory = join(root, suffix);
    const resource = `fake-oc_${suffix}`;
    const config: GatewayConfig = {
      node: { id: `urn:onlyne:gateway:im-fake-${suffix}` },
      server: { bind: '127.0.0.1', port: 0 },
      storage_path: join(directory, 'store.json'),
      channel: {
        platform: 'fake',
        conversation: `oc_${suffix}`,
        // The real im-fake package: each node replays its own one-event script.
        script: { events: [event(`${suffix}-1`, `oc_${suffix}`)], answers: [] },
      },
    };
    // Bind first: the node advertises the port it actually holds, exactly as the CLI does.
    let handler: (request: Request) => Promise<Response> = async () => new Response('starting', { status: 503 });
    const server = Bun.serve({ hostname: '127.0.0.1', port: 0, fetch: request => handler(request) });
    if (server.port === undefined) throw new Error('the fixture server bound no TCP port');
    config.server.port = server.port;
    const gateway = await createImGateway(config, { storage: 'disk', log: () => undefined });
    handler = request => gateway.fetch(request);
    nodes.push({ gateway, config, server, resource });
  }
  await settle();

  const ports = nodes.map(node => node.server.port);
  ok('three channel nodes bind three distinct ports', ports.every(port => typeof port === 'number' && port > 0)
    && new Set(ports).size === 3);
  const identities = (await Promise.all(nodes.map(node => node.gateway.status()))).map(status => status.node);
  ok('each channel node has its own identity', new Set(identities).size === 3
    && identities.every(identity => identity.startsWith('urn:onlyne:gateway:im-fake-')));

  const stores = await Promise.all(nodes.map(async node => {
    const text = await readFile(node.config.storage_path, 'utf8');
    const parsed: unknown = JSON.parse(text);
    if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error('store is not an object');
    return parsed as Record<string, unknown>;
  }));

  for (const [index, node] of nodes.entries()) {
    const own = node.config.node.id;
    const store = stores[index]!;
    const resourceKeys = Object.keys(store).filter(key => {
      const parts = key.split('/');
      return parts.length === 3 && parts[0] === 'resource';
    });
    ok(`node ${own} holds only its own resources`, resourceKeys.length === 1
      && resourceKeys[0]!.includes(encodeURIComponent(own)));
    const foreign = nodes.filter(other => other !== node).map(other => other.config.node.id);
    ok(`node ${own} holds no other node's identity`, foreign.every(id => !JSON.stringify(store).includes(id)));
    ok(`node ${own} holds no trust rows of its own making`, Object.keys(store).every(key => !key.startsWith('trust/')));
  }

  // One extra event on node a: no other store may grow, and no other log may see the source.
  await nodes[0]!.gateway.receive(event('extra-1', 'oc_a'));
  await settle();
  const after = await Promise.all(nodes.map(async node => JSON.parse(await readFile(node.config.storage_path, 'utf8')) as Record<string, unknown>));
  ok('an event accepted by one node never lands in another store',
    JSON.stringify(after[0]).includes('extra-1')
    && !JSON.stringify(after[1]).includes('extra-1')
    && !JSON.stringify(after[2]).includes('extra-1'));

  // Each node's inbound log names only its own source, so the extra event is visible once.
  const logs = await Promise.all(nodes.map(node =>
    node.gateway.node.resources.log(node.gateway.node.auth, { owner: node.config.node.id, id: node.resource })));
  ok('each node keeps its own event log', logs[0]!.items.length === 2 && logs[1]!.items.length === 1
    && logs[2]!.items.length === 1);
} finally {
  for (const node of nodes) {
    await node.gateway.stop();
    node.server.stop(true);
  }
  await rm(root, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`im-gateway channels: ${failures} check(s) failed`);
  process.exit(1);
}
console.log('im-gateway channels: all checks passed');
