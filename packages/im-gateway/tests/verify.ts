import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { MemoryStorage, OnlyneError, OnlyneNode, type Transport } from '@onlyne/lib';
import type { Connector, ImEvent, SendOutcome, SendRequest } from '@onlyne/im-core';
import { createImGateway, nodeClock, type GatewayConfig, type ImGateway } from '../src/index.js';

let failures = 0;
const ok = (label: string, condition: boolean): void => {
  if (condition) console.log(`ok - ${label}`);
  else { failures++; console.error(`not ok - ${label}`); }
};
const settle = () => new Promise(resolve => setTimeout(resolve, 20));

/** An injected connector for a platform with no package: the test emits events itself. */
class ScriptedConnector implements Connector {
  readonly platform: string;
  readonly sends: Array<{ conversation: string; text: string; reply_to?: string; attachments: string[] }> = [];
  #listeners: Array<(event: ImEvent) => void> = [];
  constructor(platform: string) { this.platform = platform; }
  async start(): Promise<void> {}
  onEvent(listener: (event: ImEvent) => void): void { this.#listeners.push(listener); }
  emit(event: ImEvent): void { for (const listener of this.#listeners) listener(event); }
  async send(request: SendRequest): Promise<SendOutcome> {
    this.sends.push({
      conversation: request.conversation,
      text: request.text,
      ...(request.reply_to === undefined ? {} : { reply_to: request.reply_to }),
      attachments: request.attachments.map(attachment => attachment.path),
    });
    return { state: 'committed', message_id: 'om_scripted' };
  }
  async stop(): Promise<void> {}
}

const platformEvent = (platform: string, source: string, conversation: string, extra: Partial<ImEvent> = {}): ImEvent => ({
  platform, source_id: source, conversation, kind: 'message',
  from: 'ou_sender', text: '你好', attachments: [], message_handle: `fk_${source}`, at: new Date().toISOString(),
  ...extra,
});

const NODE = 'urn:onlyne:gateway:im';
const AGENT = 'urn:onlyne:agent:lazuli';
const GHOST = 'urn:onlyne:agent:ghost';
const dir = await mkdtemp(join(tmpdir(), 'im-gateway-'));
const config: GatewayConfig = {
  node: { id: NODE },
  server: { bind: '127.0.0.1', port: 3993 },
  storage_path: join(dir, 'store.json'),
  connectors: [
    {
      platform: 'fake', resource_ref: { id: 'chat-01' }, conversation: 'oc_test', assignee: AGENT,
      // The real im-fake package: two replays of one platform event prove the dedup path,
      // and one queued answer is what the send operation must consume.
      script: {
        events: [
          platformEvent('fake', 'p-1', 'oc_test'),
          platformEvent('fake', 'p-1', 'oc_test'),
          platformEvent('fake', 'p-2', 'oc_test'),
        ],
        answers: [{ state: 'committed', message_id: 'om_scripted' }],
      },
    },
    { platform: 'ghost', resource_ref: { id: 'chat-ghost' }, conversation: 'oc_ghost', assignee: GHOST },
    { platform: 'script', resource_ref: { id: 'chat-render' }, conversation: 'oc_render', assignee: AGENT },
  ],
};
const endpoint = 'http://127.0.0.1:3993/onlyne/v3';
const call = async (gateway: ImGateway, method: string, path: string, body?: unknown, actor: string = AGENT): Promise<{ status: number; body: Record<string, unknown> }> => {
  const response = await gateway.node.fetch(new Request(endpoint + path, {
    method,
    headers: { 'content-type': 'application/json', 'Onlyne-Actor': actor },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  }));
  const text = await response.text();
  return { status: response.status, body: (text === '' ? {} : JSON.parse(text)) as Record<string, unknown> };
};
const mutation = (): { idempotency_key: string; retry_until: string } => ({
  idempotency_key: crypto.randomUUID(),
  retry_until: new Date(Date.now() + 60_000).toISOString(),
});

/** The reachable peer: an in-process Onlyne node the gateway delivers its notify messages to. */
const agent = new OnlyneNode({
  node_id: AGENT,
  base_url: 'http://agent.test/onlyne/v3',
  plugins: {
    storage: new MemoryStorage(),
    authenticate: async request => {
      const actor = request.headers.get('Onlyne-Actor');
      return actor === null || actor.length === 0 ? null : { actor };
    },
    resolve: async () => { throw new OnlyneError('not_found', 'The test agent resolves nobody.'); },
    decide: () => { throw new OnlyneError('decision_unavailable', 'The test agent decides nothing.'); },
    maintenanceClock: nodeClock(),
    runtime: { notify: () => undefined, supports: async () => false, inject: async () => { throw new OnlyneError('unsupported_delivery', 'no context'); } },
  },
  now: () => Date.now(),
  id: () => crypto.randomUUID(),
});
/** Routes the gateway's outbound calls into the in-process agent; any other peer is unreachable. */
const transport: Transport = {
  async request<T>(target: string, method: string, path: string, body?: unknown): Promise<T> {
    if (target !== AGENT) throw new OnlyneError('unavailable', `The test routes no peer ${target}.`);
    const response = await agent.fetch(new Request(`http://agent.test/onlyne/v3${path}`, {
      method,
      headers: { 'content-type': 'application/json', 'Onlyne-Actor': NODE },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    }));
    const text = await response.text();
    if (!response.ok) {
      const failure = (text === '' ? {} : JSON.parse(text)) as { error?: { code?: string; message?: string } };
      throw new OnlyneError((failure.error?.code ?? 'unavailable') as 'unavailable', failure.error?.message ?? 'The agent refused.');
    }
    return (text === '' ? undefined : JSON.parse(text)) as T;
  },
};

const connectors = new Map<string, ScriptedConnector>();
const factory = async (_config: GatewayConfig, entry: { platform: string; resource_ref: { id: string } }): Promise<ScriptedConnector> => {
  const connector = new ScriptedConnector(entry.platform);
  connectors.set(entry.resource_ref.id, connector);
  return connector;
};
const logs: Array<{ message: string; fields?: Record<string, unknown> }> = [];
const gateway = await createImGateway(config, {
  storage: 'disk',
  factories: { ghost: factory, script: factory },
  transport,
  log: (message, fields) => logs.push({ message, fields }),
});
await settle();
// The render target gets its own inbound event, so its projection exists to reply to.
connectors.get('chat-render')!.emit(platformEvent('script', 'r-1', 'oc_render', {
  attachments: [{ kind: 'image', platform_handle: 'fk_img' }],
}));
await settle();

// 1. The configured resource exists and the gateway serves it over HTTP.
const described = await call(gateway, 'GET', `/resources/${NODE}/chat-01`);
ok('the configured resource is registered and served', described.status === 200
  && (described.body.resource_ref as Record<string, string>).id === 'chat-01'
  && (described.body.data as Record<string, string>).conversation === 'oc_test');

// 2. A send operation goes through the driver bridge into the connector.
const sent = await call(gateway, 'POST', `/resources/${NODE}/chat-01/operations`, {
  ...mutation(), operation_id: 'op-1', operation: 'send', data: { text: '答复' },
});
ok('send commits through the im-fake connector', sent.status === 201 && sent.body.state === 'committed'
  && (sent.body.data as Record<string, string>).message_id === 'om_scripted');

// 3. The scripted inbound events were stored once each (the replay is a duplicate) and pushed
// into the conversation's own thread.
const events = await call(gateway, 'GET', `/resources/${NODE}/chat-01/events`);
const items = events.body.items as Array<Record<string, string>>;
ok('the inbound events are readable from GET events', events.status === 200 && items.length === 2
  && items[0]?.source_id === 'p-1' && items[1]?.source_id === 'p-2');
const owed = await gateway.storage.transaction(async tx => tx.scan('im/push/'));
ok('an accepted push leaves no owed row', owed.length === 0);
const grants = await agent.threads.grants(agent.auth);
// The agent is the assignee of two conversations, so it holds one grant per thread.
ok('the recipient learned each conversation thread through a grant notice', grants.items.length === 2);
const threads = await gateway.node.threads.list(gateway.node.auth);
const conversationThread = threads.items.find(thread => thread.thread_ref.id === 'im-chat-01');
ok('the gateway owns one thread per conversation', conversationThread !== undefined
  && threads.items.filter(thread => thread.thread_ref.id.startsWith('im-')).length === 3);
if (conversationThread !== undefined) {
  const messages = await gateway.node.messages.list(gateway.node.auth, conversationThread.thread_ref, { attention: 'thread' });
  const first = messages.items[0];
  const second = messages.items[1];
  const data = first?.message.meta?.data as
    { resource_ref?: { id?: string }; event_id?: string; source_id?: string; from?: string } | undefined;
  ok('both events land in that one thread, in arrival order',
    messages.items.length === 2 && first?.seq === '1' && second?.seq === '2');
  ok('the notify body is the platform text and the coordinates ride along',
    first?.message.msg === '你好' && first?.message.delivery_kind === 'notify'
    && first?.message.meta?.thread?.id === 'im-chat-01'
    && data?.resource_ref?.id === 'chat-01' && data?.event_id === '1'
    && data?.source_id === 'p-1' && data?.from === 'ou_sender');
}

// 5. An agent reply in the conversation thread is rendered back to the platform.
const replyTarget = `im-notify-${NODE}-chat-render-1`;
const reply = await call(gateway, 'POST', `/threads/${NODE}/im-chat-render/messages`, {
  ...mutation(),
  message: {
    id: 'agent-reply-1', sender: AGENT, delivery_kind: 'reply', msg: '收到，我来处理',
    meta: { in_reply_to: { sender: NODE, id: replyTarget } },
  },
  target_endpoint: NODE,
  plan: { response: 'none', execution: 'existing' },
});
ok('the agent reply lands in the conversation thread', reply.status === 201);
await gateway.maintain();
const scripted = connectors.get('chat-render')!;
ok('the reply is rendered to the platform, quoting the inbound message',
  scripted.sends.length === 1 && scripted.sends[0]?.text === '收到，我来处理'
  && scripted.sends[0]?.reply_to === 'fk_r-1' && scripted.sends[0]?.conversation === 'oc_render');
await gateway.maintain();
ok('a second pass renders nothing twice', scripted.sends.length === 1);
const renderCursor = await gateway.storage.transaction(async tx => tx.get<{ seq: string }>(`im/render/${encodeURIComponent(NODE)}/chat-render`));
ok('the render cursor advanced past the reply', renderCursor?.seq === '2');

// A reply whose send was interrupted stays marked `rendering`: the platform cannot deduplicate
// a re-send, so the pass reports it and moves on instead of posting a second copy.
const secondReply = await call(gateway, 'POST', `/threads/${NODE}/im-chat-render/messages`, {
  ...mutation(),
  message: { id: 'agent-reply-2', sender: AGENT, delivery_kind: 'reply', msg: '第二条' },
  target_endpoint: NODE,
  plan: { response: 'none', execution: 'existing' },
});
ok('a second agent reply lands', secondReply.status === 201);
await gateway.storage.transaction(async tx => {
  await tx.put(`im/render/${encodeURIComponent(NODE)}/chat-render/seq/3`, { state: 'rendering', at: new Date().toISOString() });
});
await gateway.maintain();
ok('an interrupted render is not sent twice', scripted.sends.length === 1
  && logs.some(entry => entry.message === 'im.render.ambiguous'));

// The projection carries the platform payload: attachments, so no second read is needed.
const renderThread = await gateway.node.messages.list(gateway.node.auth, { owner: NODE, id: 'im-chat-render' }, { attention: 'thread' });
const projection = renderThread.items.find(entry => entry.message.delivery_kind === 'notify');
const projectionData = projection?.message.meta?.data as
  { attachments?: Array<{ platform_handle?: string }> } | undefined;
ok('the projection carries the attachment list', projectionData?.attachments?.[0]?.platform_handle === 'fk_img');

// RFC 0006 §1: a query operation answers from the endpoint's own state.
const queried = await call(gateway, 'POST', `/resources/${NODE}/chat-01/operations`, {
  ...mutation(), operation_id: 'op-read-1', operation: 'read', data: { event_id: '1' },
});
ok('a read operation answers from the endpoint state', queried.status === 201
  && queried.body.state === 'committed'
  && (queried.body.data as { source_id?: string }).source_id === 'p-1');

// Inline bytes are stored on the host, and the connector receives that path.
const withFile = await call(gateway, 'POST', `/resources/${NODE}/chat-render/operations`, {
  ...mutation(), operation_id: 'op-file-1', operation: 'send',
  data: { text: '带图', attachments: [{ kind: 'image', name: 'a.png', content: btoa('png-bytes') }] },
});
const filePath = scripted.sends.at(-1)?.attachments[0];
ok('an inline attachment is stored and handed to the connector', withFile.status === 201
  && filePath !== undefined && filePath.endsWith('a.png')
  && await readFile(filePath, 'utf8') === 'png-bytes');

// 4. An unreachable recipient keeps the push owed and the ring retries it.
connectors.get('chat-ghost')!.emit(platformEvent('ghost', 'g-1', 'oc_ghost'));
await settle();
const owedGhost = await gateway.storage.transaction(async tx => tx.scan('im/push/'));
ok('an unreachable recipient leaves the push owed and logs it', owedGhost.length === 1
  && logs.some(entry => entry.message === 'im.push.refused'));
await gateway.maintain();
const stillOwed = await gateway.storage.transaction(async tx => tx.scan('im/push/'));
ok('the maintenance ring retries the owed push', stillOwed.length === 1);

// 6. The registry survives a gateway rebuild over the same store.
await gateway.stop();
const second = await createImGateway(config, {
  factories: { ghost: factory, script: factory }, transport, log: () => undefined,
});
const persisted = await call(second, 'GET', `/resources/${NODE}/chat-01`);
ok('the resource registration persists across a rebuild', persisted.status === 200
  && (persisted.body.resource_ref as Record<string, string>).id === 'chat-01');
const persistedEvents = await call(second, 'GET', `/resources/${NODE}/chat-01/events`);
ok('stored events persist across a rebuild', (persistedEvents.body.items as Array<Record<string, string>>)[0]?.source_id === 'p-1');
await second.stop();

await rm(dir, { recursive: true, force: true });

if (failures > 0) {
  console.error(`im-gateway verify: ${failures} check(s) failed`);
  process.exit(1);
}
console.log('im-gateway verify: all checks passed');
