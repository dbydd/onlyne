import { OnlyneNode, type Auth, type Json, type Plugins, type ResourceDriver, type ResourceRef } from '@onlyne/lib';
import { MemoryStorage } from '@onlyne/lib/memory';
import { createDriverBridge, type Connector, type ImEvent, type SendOutcome, type SendRequest } from '../src/index.js';

let failures = 0;
const ok = (label: string, condition: boolean) => {
  if (condition) console.log(`ok   ${label}`);
  else { failures += 1; console.error(`FAIL ${label}`); }
};

/** A scripted connector: events pushed by the test, sends answered from a queue. */
class ScriptedConnector implements Connector {
  readonly platform: string;
  private listeners: Array<(event: ImEvent) => void> = [];
  sends: Array<{ conversation: string; text: string; reply_to?: string; reply_in_thread?: boolean; attachments: string[] }> = [];
  answers: Array<SendOutcome> = [];
  constructor(platform: string) { this.platform = platform; }
  async start(): Promise<void> {}
  onEvent(listener: (event: ImEvent) => void): void { this.listeners.push(listener); }
  emit(event: ImEvent): void { for (const listener of this.listeners) listener(event); }
  async send(request: SendRequest): Promise<SendOutcome> {
    this.sends.push({
      conversation: request.conversation,
      text: request.text,
      ...(request.reply_to === undefined ? {} : { reply_to: request.reply_to }),
      ...(request.reply_in_thread === true ? { reply_in_thread: true } : {}),
      attachments: request.attachments.map(attachment => attachment.path),
    });
    const answer = this.answers.shift();
    if (answer === undefined) throw new Error('The test scripted no answer.');
    return answer;
  }
  async stop(): Promise<void> {}
}

const NODE = 'urn:onlyne:gateway:im';
const ASSIGNEE = 'urn:onlyne:agent:lazuli';
const REF: ResourceRef = { owner: NODE, id: 'chat-01' };

const plugins: Omit<Plugins, 'storage' | 'resources'> = {
  authenticate: async () => null,
  runtime: {
    notify: () => undefined,
    supports: async () => false,
    inject: async () => { throw new Error('unreached'); },
  },
  resolve: async () => { throw new Error('unreached'); },
  decide: async () => { throw new Error('unreached'); },
};
const connector = new ScriptedConnector('lark');
const storage = new MemoryStorage();
const stored: Array<{ name: string; text: string }> = [];
// The bridge resolves the resource service lazily: the library captures the driver when the
// service is constructed (resources.ts:159), so the host builds the bridge first.
const bridge = createDriverBridge({
  node: NODE,
  resources: () => node.resources,
  storage: { transaction: work => storage.transaction(work) },
  connectors: [{ ref: REF, connector }],
  writeAttachment: async (_ref, name, bytes) => {
    stored.push({ name, text: new TextDecoder().decode(bytes) });
    return `/tmp/${name}`;
  },
});
const node = new OnlyneNode({
  node_id: NODE,
  base_url: 'http://127.0.0.1:3999/onlyne/v3',
  plugins: { ...plugins, storage, resources: bridge },
  now: () => Date.now(),
  id: () => Math.random().toString(36).slice(2),
});

const resource = await node.resources.register({ actor: NODE } satisfies Auth, {
  resource_ref: REF,
  kind: 'im_conversation',
  operations: ['read', 'send', 'list_events', 'handover', 'describe'],
  assignee: ASSIGNEE,
  revision: 'rev-a',
  readers: [],
  controllers: [],
  event_recipients: [ASSIGNEE],
  result_recipients: [ASSIGNEE],
  event_seq: '0',
  retention_ms: 3_600_000,
  data: { conversation: 'oc_test' },
});
ok('register returns the normalized resource', resource.assignee === ASSIGNEE && resource.readers.includes(ASSIGNEE));

const event = (id: string, text: string): ImEvent => ({
  platform: 'lark', source_id: id, conversation: 'oc_test', kind: 'message',
  from: 'ou_sender', text, attachments: [], message_handle: `om_${id}`, at: '2026-10-26T12:00:00Z',
});

await bridge.receive(event('p-1', '第一条'));
const first = await node.resources.events({ actor: ASSIGNEE }, REF, {});
ok('one event lands', first.items.length === 1 && first.items[0]!.source_id === 'p-1');

await bridge.receive(event('p-1', '第一条'));
const afterDuplicate = await node.resources.events({ actor: ASSIGNEE }, REF, {});
ok('duplicate source_id produces no second event', afterDuplicate.items.length === 1);

await bridge.receive(event('p-2', '回复目标'));
const second = await node.resources.events({ actor: ASSIGNEE }, REF, {});
const secondId = second.items[1]!.event_id;
ok('second event gets the next event_id', secondId === '2');

const assignee: Auth = { actor: ASSIGNEE };
const mutation = () => ({
  idempotency_key: 'm-' + Math.random().toString(36).slice(2),
  retry_until: new Date(Date.now() + 3_600_000).toISOString(),
});
const deadline = () => new Date(Date.now() + 3_600_000).toISOString();

connector.answers.push({ state: 'committed', message_id: 'om_1', ref_handle: 'om_1' });
const committed = await node.resources.operate(assignee, REF, {
  operation_id: 'op-1', operation: 'send',
  data: { text: '答复', reply_to_event: secondId },
  ...mutation(),
});
ok('send commits with the platform message id', committed.state === 'committed' && (committed.data as Record<string, string>).message_id === 'om_1');
ok('the connector received the platform reply handle', connector.sends[0]?.reply_to === 'om_p-2');

connector.answers.push({ state: 'committed', error: 'platform said no' });
const rejected = await node.resources.operate(assignee, REF, {
  operation_id: 'op-2', operation: 'send',
  data: { text: '被拒' },
  ...mutation(),
});
ok('a platform rejection aborts with the source text', rejected.state === 'aborted'
  && (rejected.error?.details as Record<string, string> | undefined)?.source === 'platform said no');

connector.answers.push({ state: 'unknown' });
const unclear = await node.resources.operate(assignee, REF, {
  operation_id: 'op-3', operation: 'send',
  data: { text: '结果不明' },
  ...mutation(),
});
ok('an unclear outcome reports unknown', unclear.state === 'unknown');

try {
  await node.resources.operate(assignee, REF, {
    operation_id: 'op-4', operation: 'send',
    data: { text: '' },
    ...mutation(),
  });
  ok('empty text is rejected before the connector', false);
} catch (error) {
  ok('empty text is rejected before the connector', (error as { error?: { code?: string } }).error?.code === 'invalid_request');
  ok('the connector saw no fourth send', connector.sends.length === 3);
}

// `reply_in_thread` and attachments survive the bridge; inline bytes become a host path.
connector.answers.push({ state: 'committed', message_id: 'om_thread' });
const threaded = await node.resources.operate(assignee, REF, {
  operation_id: 'op-7', operation: 'send',
  data: { text: '线程内回复', reply_in_thread: true, reply_to_event: secondId },
  ...mutation(),
});
ok('reply_in_thread reaches the connector', threaded.state === 'committed'
  && connector.sends.at(-1)?.reply_in_thread === true && connector.sends.at(-1)?.reply_to === 'om_p-2');

connector.answers.push({ state: 'committed', message_id: 'om_file' });
const withFile = await node.resources.operate(assignee, REF, {
  operation_id: 'op-8', operation: 'send',
  data: { text: '带图', attachments: [{ kind: 'image', name: 'chart.png', content: btoa('fake-png-bytes') }] },
  ...mutation(),
});
ok('inline content is stored by the host and passed as a path', withFile.state === 'committed'
  && stored.length === 1 && stored[0]?.name === 'chart.png' && stored[0]?.text === 'fake-png-bytes'
  && connector.sends.at(-1)?.attachments[0] === '/tmp/chart.png');

let mixed = false;
try {
  await node.resources.operate(assignee, REF, {
    operation_id: 'op-9', operation: 'send',
    data: { text: 'x', attachments: [{ kind: 'image', name: 'a.png', path: '/a', content: 'eA==' }] },
    ...mutation(),
  });
} catch (error) {
  mixed = (error as { error?: { code?: string } }).error?.code === 'invalid_request';
}
ok('an attachment carrying both a path and content is refused', mixed);

// A host without the write hook refuses inline bytes instead of silently dropping them.
const hookless = createDriverBridge({
  node: NODE,
  resources: () => node.resources,
  storage: { transaction: work => storage.transaction(work) },
  connectors: [{ ref: REF, connector }],
});
const unhooked = await hookless.execute(resource, {
  operation_id: 'op-10', operation: 'send', ...mutation(),
  data: { attachments: [{ kind: 'file', name: 'a.txt', content: 'eA==' }] },
});
ok('inline content without a host hook is refused', unhooked.state === 'aborted'
  && unhooked.error?.code === 'unsupported_delivery');

// Backlog fence: an unacknowledged event blocks the handover commit.
await bridge.receive(event('p-3', '积压'));
const initiate = await node.resources.operate(assignee, REF, {
  operation_id: 'op-5', operation: 'handover',
  data: { target: 'urn:onlyne:agent:next', decision_deadline: deadline() },
  ...mutation(),
});
ok('handover initiates pending', initiate.state === 'pending');
const events = await node.resources.events(assignee, REF, {});
const backlogId = events.items[2]!.event_id;
// The target accepts; the commit hits the backlog fence and the error surfaces as `conflict`.
let fenced = false;
try {
  await node.resources.decideHandover({ actor: 'urn:onlyne:agent:next' }, REF, 'op-5', true, mutation());
} catch (error) {
  fenced = (error as { error?: { code?: string } }).error?.code === 'conflict';
}
ok('the backlog fence refuses the commit with conflict', fenced);
// Clearing the whole backlog lets a fresh handover commit: events 1 and 2 are unacked too.
for (const item of events.items) {
  await node.resources.ack(assignee, REF, item.event_id, mutation());
}
const fresh = await node.resources.operate(assignee, REF, {
  operation_id: 'op-6', operation: 'handover',
  data: { target: 'urn:onlyne:agent:next', decision_deadline: deadline() },
  ...mutation(),
});
const decided = await node.resources.decideHandover({ actor: 'urn:onlyne:agent:next' }, REF, 'op-6', true, mutation());
ok('a cleared backlog lets the handover commit', decided.state === 'committed' && fresh.state === 'pending');

if (failures > 0) {
  console.error(`${failures} failure(s)`);
  process.exit(1);
}
console.log('im-core verify: all checks passed');
