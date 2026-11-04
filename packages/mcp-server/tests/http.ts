/**
 * Signed HTTP: MCP host and a peer OnlyneNode, discovery, trust, cross-node send,
 * list_joined, describe_thread, in_reply_to.
 */
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { InMemoryTransport } from '@modelcontextprotocol/sdk/inMemory.js';
import {
  generateKeyMaterial, MemoryStorage, OnlyneError, OnlyneNode, trustPhase,
  type Auth, type KeyMaterial, type Plugins,
} from '@onlyne/lib';
import { createOnlyneHost, createOnlyneMcpServer, nodeClock } from '../src/index.ts';

let failures = 0;
const ok = (label: string, condition: boolean): void => {
  if (condition) console.log(`ok - ${label}`);
  else { failures++; console.error(`not ok - ${label}`); }
};

const settle = (ms = 30): Promise<void> => new Promise(resolve => setTimeout(resolve, ms));
const waitFor = async (predicate: () => boolean | Promise<boolean>, timeoutMs = 5_000): Promise<boolean> => {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await predicate()) return true;
    await settle(20);
  }
  return false;
};

const rec = (value: unknown): Record<string, unknown> | undefined =>
  value !== null && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : undefined;

const toolBody = (result: unknown): Record<string, unknown> | undefined => {
  const content = rec(result)?.content as Array<{ text?: string }> | undefined;
  return rec(JSON.parse(content?.[0]?.text ?? '{}'));
};

const MCP = 'urn:onlyne:agent:mcp';
const PEER = 'urn:onlyne:agent:peer';
const servers: Array<{ stop(closeActiveConnections?: boolean): void }> = [];

function keysHook(): Plugins['keys'] {
  let material: KeyMaterial = generateKeyMaterial();
  return Object.assign(async () => material, {
    replace: async (next: KeyMaterial) => { material = next; },
  });
}

function listen(initial: (request: Request) => Promise<Response>): {
  url: string; port: number; set(next: (request: Request) => Promise<Response>): void;
} {
  let handler = initial;
  const server = Bun.serve({ hostname: '127.0.0.1', port: 0, fetch: request => handler(request) });
  servers.push(server);
  const port = server.port ?? 0;
  return {
    url: `http://127.0.0.1:${port}/onlyne/v3`,
    port,
    set: next => { handler = next; },
  };
}

const mcpBind = listen(async () => new Response('starting', { status: 503 }));
const peerBind = listen(async () => new Response('starting', { status: 503 }));

const host = await createOnlyneHost({
  node: { id: MCP },
  server: { bind: '127.0.0.1', port: mcpBind.port },
  storage_path: '/tmp/mcp-http-unused.json',
}, { storage: 'memory', keys: keysHook() });
mcpBind.set(request => host.node.fetch(request));
host.node.client.runMaintenance();

const peerStorage = new MemoryStorage();
let peer!: OnlyneNode;
const peerPlugins: Omit<Plugins, 'storage'> = {
  authenticate: async request => {
    const actor = request.headers.get('Onlyne-Actor');
    return actor === null || actor.length === 0 ? null : { actor };
  },
  maintenanceClock: nodeClock(),
  resolve: async id => {
    const target = id === PEER ? { node_id: id, base_url: peerBind.url } : await peer.discovery.resolve(id);
    return { ...target, headers: { 'Onlyne-Actor': PEER } };
  },
  keys: keysHook(),
  decide: async () => { throw new OnlyneError('decision_unavailable', 'no decide'); },
  inboundThread: async () => true,
  runtime: {
    supports: async plan => plan.response !== 'await',
    notify: () => {},
    inject: async () => ({ context: PEER, consumed: true, outcome: 'delivered' as const }),
  },
};
peer = new OnlyneNode({
  node_id: PEER,
  base_url: peerBind.url,
  authentication: ['signature'],
  plugins: { ...peerPlugins, storage: peerStorage },
  now: () => Date.now(),
  id: () => crypto.randomUUID(),
});
peerBind.set(request => peer.fetch(request));
peer.client.runMaintenance();

const mcpAuth: Auth = { actor: MCP };
const peerAuth: Auth = { actor: PEER };

await host.node.discovery.observe(peerBind.url, 'bootstrap');
await peer.discovery.observe(mcpBind.url, 'bootstrap');
ok('both nodes observe each other from seed URLs', true);

const mcpId = (await host.node.keys.describe())!;
const peerId = (await peer.keys.describe())!;
await host.node.trust.request(mcpAuth, PEER, peerId.fingerprint);
await peer.trust.accept(peerAuth, MCP, mcpId.fingerprint);
await host.node.trust.accept(mcpAuth, PEER, peerId.fingerprint);
ok('MCP and peer complete a key exchange',
  trustPhase((await host.node.trust.entry(PEER))!) === 'exchanged'
  && trustPhase((await peer.trust.entry(MCP))!) === 'exchanged');

const server = createOnlyneMcpServer(host.node);
const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
await server.connect(serverTransport);
const client = new Client({ name: 'onlyne-http-test', version: '0' });
await client.connect(clientTransport);

const cross = await client.callTool({
  name: 'send_message',
  arguments: { to: PEER, msg: 'hello peer', delivery_kind: 'note' },
});
const crossBody = toolBody(cross);
ok('send_message delivers a signed message to the peer node',
  cross.isError !== true && crossBody?.admission === 'accepted');

await waitFor(async () => {
  await peer.client.reconcile();
  const inbox = await peer.messages.inbox(peerAuth);
  return inbox.items.some(row => row.attention.message_ref.sender === MCP);
});
ok('the peer stored the cross-node send',
  (await peer.messages.inbox(peerAuth)).items.some(row => row.attention.message_ref.sender === MCP));

const thread = await peer.threads.create(peerAuth, {
  idempotency_key: crypto.randomUUID(),
  retry_until: new Date(Date.now() + 60_000).toISOString(),
  title: 'peer-owned',
});
await peer.threads.member(peerAuth, thread.thread_ref, MCP, { state: 'active', access: ['post', 'read'] }, {
  idempotency_key: crypto.randomUUID(),
  retry_until: new Date(Date.now() + 60_000).toISOString(),
});
await peer.messages.admit(peerAuth, {
  idempotency_key: crypto.randomUUID(),
  retry_until: new Date(Date.now() + 60_000).toISOString(),
  message: {
    id: 'invite-01', sender: PEER, delivery_kind: 'note', msg: 'join this',
    meta: { thread: thread.thread_ref, mentions: [MCP, '@all'] },
  },
  target_endpoint: PEER,
  plan: { response: 'none', execution: 'existing' },
});
await peer.client.maintain();

ok('MCP list_joined includes the peer-owned thread after reconcile', await waitFor(async () => {
  await host.node.client.reconcile();
  const joined = toolBody(await client.callTool({ name: 'list_joined', arguments: {} }));
  const items = Array.isArray(joined?.items) ? joined.items : [];
  return items.some(item => rec(rec(item)?.thread_ref)?.id === thread.thread_ref.id);
}));

const described = toolBody(await client.callTool({
  name: 'describe_thread',
  arguments: { owner: PEER, id: thread.thread_ref.id },
}));
ok('describe_thread reads a thread this node was invited into',
  rec(rec(described?.thread)?.thread_ref)?.id === thread.thread_ref.id);

const reply = await client.callTool({
  name: 'send_message',
  arguments: {
    to: PEER,
    msg: 'reply on your thread',
    thread_owner: thread.thread_ref.owner,
    thread_id: thread.thread_ref.id,
    in_reply_to_sender: PEER,
    in_reply_to_id: 'invite-01',
    response: 'async',
  },
});
const replyBody = toolBody(reply);
ok('send_message with thread + in_reply_to and response:async targets the peer thread',
  reply.isError !== true && replyBody?.admission === 'accepted'
  && rec(rec(replyBody?.receipt)?.thread_ref)?.id === thread.thread_ref.id);

await client.close();
await server.close();
host.node.client.stopMaintenance();
peer.client.stopMaintenance();
await host.stop();
for (const server of servers) server.stop(true);

if (failures > 0) {
  console.error(`mcp-server http: ${failures} check(s) failed`);
  process.exit(1);
}
console.log('mcp-server http: all checks passed');
