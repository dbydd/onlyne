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
const NEXT = 'urn:onlyne:agent:next';
const dir = await mkdtemp(join(tmpdir(), 'im-gateway-'));
const config: GatewayConfig = {
  node: { id: NODE },
  server: { bind: '127.0.0.1', port: 3993 },
  storage_path: join(dir, 'store.json'),
  connectors: [
    {
      platform: 'fake', conversation: 'oc_test', assignee: AGENT,
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
    { platform: 'ghost', conversation: 'oc_ghost', assignee: GHOST },
    { platform: 'script', conversation: 'oc_render', assignee: AGENT },
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

/** A reachable peer: an in-process Onlyne node the gateway delivers to and probes. */
const peer = (node_id: string, base_url: string): OnlyneNode => new OnlyneNode({
  node_id,
  base_url,
  plugins: {
    storage: new MemoryStorage(),
    authenticate: async request => {
      const actor = request.headers.get('Onlyne-Actor');
      return actor === null || actor.length === 0 ? null : { actor };
    },
    resolve: async () => { throw new OnlyneError('not_found', 'The test peer resolves nobody.'); },
    decide: () => { throw new OnlyneError('decision_unavailable', 'The test peer decides nothing.'); },
    maintenanceClock: nodeClock(),
    runtime: { notify: () => undefined, supports: async () => false, inject: async () => { throw new OnlyneError('unsupported_delivery', 'no context'); } },
  },
  now: () => Date.now(),
  id: () => crypto.randomUUID(),
});
const agent = peer(AGENT, 'http://agent.test/onlyne/v3');
// The handover target is a real node, so the probe and the push after the commit both run.
const next = peer(NEXT, 'http://next.test/onlyne/v3');
const peers = new Map([
  [AGENT, { node: agent, base_url: 'http://agent.test/onlyne/v3' }],
  [NEXT, { node: next, base_url: 'http://next.test/onlyne/v3' }],
]);
/** Routes the gateway's outbound calls into the in-process peers; any other peer is unreachable. */
const transport: Transport = {
  async request<T>(target: string, method: string, path: string, body?: unknown): Promise<T> {
    const routed = peers.get(target);
    if (routed === undefined) throw new OnlyneError('unavailable', `The test routes no peer ${target}.`);
    const response = await routed.node.fetch(new Request(`${routed.base_url}${path}`, {
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
const factory = async (_config: GatewayConfig, entry: { platform: string; conversation?: string }): Promise<ScriptedConnector> => {
  const connector = new ScriptedConnector(entry.platform);
  connectors.set(entry.conversation ?? '', connector);
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
connectors.get('oc_render')!.emit(platformEvent('script', 'r-1', 'oc_render', {
  attachments: [{ kind: 'image', platform_handle: 'fk_img' }],
}));
await settle();

// 1. The configured resource exists and the gateway serves it over HTTP.
const described = await call(gateway, 'GET', `/resources/${NODE}/fake-oc_test`);
ok('the configured resource is registered and served', described.status === 200
  && (described.body.resource_ref as Record<string, string>).id === 'fake-oc_test'
  && (described.body.data as Record<string, string>).conversation === 'oc_test'
  && (described.body.binding as { handler?: string } | undefined)?.handler === AGENT
  && (described.body.log_readers as string[] | undefined)?.includes(NODE) === true);

// 2. A send operation goes through the driver bridge into the connector.
const sent = await call(gateway, 'POST', `/resources/${NODE}/fake-oc_test/operations`, {
  ...mutation(), operation_id: 'op-1', operation: 'send', data: { text: '答复' },
});
ok('send commits through the im-fake connector', sent.status === 201 && sent.body.state === 'committed'
  && (sent.body.data as Record<string, string>).message_id === 'om_scripted');

// 3. The scripted inbound events were stored once each (the replay is a duplicate) and pushed
// into the conversation's own thread.
const events = await call(gateway, 'GET', `/resources/${NODE}/fake-oc_test/log`);
const items = events.body.items as Array<Record<string, string>>;
ok('the inbound events are readable from GET log', events.status === 200 && items.length === 2
  && items[0]?.source_id === 'p-1' && items[1]?.source_id === 'p-2');
const owed = await gateway.storage.transaction(async tx => tx.scan('im/push/'));
ok('an accepted push leaves no owed row', owed.length === 0);
const grants = await agent.threads.grants(agent.auth);
// The agent is the assignee of two conversations, so it holds one grant per thread.
ok('the recipient learned each conversation thread through a grant notice', grants.items.length === 2);
const threads = await gateway.node.threads.list(gateway.node.auth);
const conversationThread = threads.items.find(thread => thread.thread_ref.id === 'im-fake-oc_test');
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
    && first?.message.meta?.thread?.id === 'im-fake-oc_test'
    && data?.resource_ref?.id === 'fake-oc_test' && data?.event_id === '1'
    && data?.source_id === 'p-1' && data?.from === 'ou_sender');
}

// 5. An agent reply in the conversation thread is rendered back to the platform.
const replyTarget = `im-notify-${NODE}-script-oc_render-1`;
const reply = await call(gateway, 'POST', `/threads/${NODE}/im-script-oc_render/messages`, {
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
const scripted = connectors.get("oc_render")!;
ok('the reply is rendered to the platform, quoting the inbound message',
  scripted.sends.length === 1 && scripted.sends[0]?.text === '收到，我来处理'
  && scripted.sends[0]?.reply_to === 'fk_r-1' && scripted.sends[0]?.conversation === 'oc_render');
await gateway.maintain();
ok('a second pass renders nothing twice', scripted.sends.length === 1);
const renderCursor = await gateway.storage.transaction(async tx => tx.get<{ seq: string }>(`im/render/${encodeURIComponent(NODE)}/script-oc_render`));
ok('the render cursor advanced past the reply', renderCursor?.seq === '2');

// A reply whose send was interrupted stays marked `rendering`: the platform cannot deduplicate
// a re-send, so the pass reports it and moves on instead of posting a second copy.
const secondReply = await call(gateway, 'POST', `/threads/${NODE}/im-script-oc_render/messages`, {
  ...mutation(),
  message: { id: 'agent-reply-2', sender: AGENT, delivery_kind: 'reply', msg: '第二条' },
  target_endpoint: NODE,
  plan: { response: 'none', execution: 'existing' },
});
ok('a second agent reply lands', secondReply.status === 201);
await gateway.storage.transaction(async tx => {
  await tx.put(`im/render/${encodeURIComponent(NODE)}/script-oc_render/seq/3`, { state: 'rendering', at: new Date().toISOString() });
});
await gateway.maintain();
ok('an interrupted render is not sent twice', scripted.sends.length === 1
  && logs.some(entry => entry.message === 'im.render.ambiguous'));

// The projection carries the platform payload: attachments, so no second read is needed.
const renderThread = await gateway.node.messages.list(gateway.node.auth, { owner: NODE, id: 'im-script-oc_render' }, { attention: 'thread' });
const projection = renderThread.items.find(entry => entry.message.delivery_kind === 'notify');
const projectionData = projection?.message.meta?.data as
  { attachments?: Array<{ platform_handle?: string }> } | undefined;
ok('the projection carries the attachment list', projectionData?.attachments?.[0]?.platform_handle === 'fk_img');

// RFC 0006 §1: a query operation answers from the endpoint's own state.
const queried = await call(gateway, 'POST', `/resources/${NODE}/fake-oc_test/operations`, {
  ...mutation(), operation_id: 'op-read-1', operation: 'read', data: { event_id: '1' },
});
ok('a read operation answers from the endpoint state', queried.status === 201
  && queried.body.state === 'committed'
  && (queried.body.data as { source_id?: string }).source_id === 'p-1');

// Inline bytes are stored on the host, and the connector receives that path.
const withFile = await call(gateway, 'POST', `/resources/${NODE}/script-oc_render/operations`, {
  ...mutation(), operation_id: 'op-file-1', operation: 'send',
  data: { text: '带图', attachments: [{ kind: 'image', name: 'a.png', content: btoa('png-bytes') }] },
});
const filePath = scripted.sends.at(-1)?.attachments[0];
ok('an inline attachment is stored and handed to the connector', withFile.status === 201
  && filePath !== undefined && filePath.endsWith('a.png')
  && await readFile(filePath, 'utf8') === 'png-bytes');

// 4. An unreachable recipient keeps the push owed and the ring retries it.
connectors.get('oc_ghost')!.emit(platformEvent('ghost', 'g-1', 'oc_ghost'));
await settle();
const owedGhost = await gateway.storage.transaction(async tx => tx.scan('im/push/'));
ok('an unreachable recipient leaves the push owed and logs it', owedGhost.length === 1
  && logs.some(entry => entry.message === 'im.push.refused'));
await gateway.maintain();
const stillOwed = await gateway.storage.transaction(async tx => tx.scan('im/push/'));
ok('the maintenance ring retries the owed push', stillOwed.length === 1);

// 4b. Handover: the ack cursor, the probe, the commit, and the push target after it.
const acked = await call(gateway, 'POST', `/resources/${NODE}/fake-oc_test/events/1/ack`, mutation());
const ackedTwo = await call(gateway, 'POST', `/resources/${NODE}/fake-oc_test/events/2/ack`, mutation());
ok('an ack advances the handler cursor', acked.status === 200 && acked.body.cursor === '1'
  && ackedTwo.body.cursor === '2');
const initiate = await call(gateway, 'POST', `/resources/${NODE}/fake-oc_test/operations`, {
  ...mutation(), operation_id: 'op-handover-1', operation: 'handover',
  data: { target: NEXT, handover_deadline: new Date(Date.now() + 300_000).toISOString() },
});
const handover = initiate.body.data as { handler?: string; cursor?: string } | undefined;
ok('the probe commits the binding to the target', initiate.status === 201 && initiate.body.state === 'committed'
  && handover?.handler === NEXT && handover?.cursor === '2');
const afterHandover = await call(gateway, 'GET', `/resources/${NODE}/fake-oc_test`, undefined, NEXT);
ok('the handler and revision moved', (afterHandover.body.binding as { handler?: string } | undefined)?.handler === NEXT
  && afterHandover.body.revision !== described.body.revision);
// RFC 0006 §6: a handover never touches `log_readers`, so the log stays readable to its standing reader.
const ownerLog = await gateway.node.resources.log(gateway.node.auth, { owner: NODE, id: 'fake-oc_test' });
ok('the gateway keeps its log read after the handover', ownerLog.items.length === 2);
const before = await call(gateway, 'GET', `/resources/${NODE}/fake-oc_test/log/1`, undefined, NEXT);
ok('the new handler reads an event received before the commit', (before.body as { source_id?: string }).source_id === 'p-1');
const staleSend = await call(gateway, 'POST', `/resources/${NODE}/fake-oc_test/operations`, {
  ...mutation(), operation_id: 'op-after-handover', operation: 'send', data: { text: '旧处理者' },
});
ok('the previous handler can no longer send', staleSend.status === 404);
// The next inbound event is pushed to the new handler, which is where the binding points.
await gateway.receive(platformEvent('fake', 'p-3', 'oc_test', { text: '交接之后' }));
await settle();
const routed = await call(gateway, 'GET', `/resources/${NODE}/fake-oc_test/log`, undefined, NEXT);
const newEvent = (routed.body.items as Array<Record<string, string>>).find(item => item.source_id === 'p-3');
ok('the new handler reads the event received after the commit', newEvent?.event_id === '3');
const nextGrants = await next.threads.grants(next.auth);
ok('the push after the handover reaches the new handler',
  nextGrants.items.some(grant => grant.thread_ref.id === 'im-fake-oc_test'));
const owedChat = await gateway.storage.transaction(async tx =>
  (await tx.scan<{ ref: { id: string } }>('im/push/')).filter(row => row.value.ref.id === 'fake-oc_test'));
ok('a delivered push leaves no owed row for the conversation', owedChat.length === 0);

// 6. The registry survives a gateway rebuild over the same store.
await gateway.stop();
const second = await createImGateway(config, {
  factories: { ghost: factory, script: factory }, transport, log: () => undefined,
});
const persisted = await call(second, 'GET', `/resources/${NODE}/fake-oc_test`, undefined, NEXT);
ok('the resource registration persists across a rebuild', persisted.status === 200
  && (persisted.body.resource_ref as Record<string, string>).id === 'fake-oc_test');
const persistedEvents = await call(second, 'GET', `/resources/${NODE}/fake-oc_test/log`, undefined, NEXT);
ok('stored events persist across a rebuild', (persistedEvents.body.items as Array<Record<string, string>>)[0]?.source_id === 'p-1');
await second.stop();

await rm(dir, { recursive: true, force: true });

if (failures > 0) {
  console.error(`im-gateway verify: ${failures} check(s) failed`);
  process.exit(1);
}
console.log('im-gateway verify: all checks passed');
