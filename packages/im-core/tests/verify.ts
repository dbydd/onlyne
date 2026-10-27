import { OnlyneNode, type Auth, type Json, type Plugins, type ResourceDriver, type ResourceRef } from '@onlyne/lib';
import { MemoryStorage } from '@onlyne/lib/memory';
import { coordinateOf, createDriverBridge, resourceIdOf, type Connector, type ImEvent, type SendOutcome, type SendRequest } from '../src/index.js';

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
const PLATFORM = 'lark';
const CONVERSATION = 'oc_test';
// RFC 0012 §2: the resource id is the platform name plus the platform coordinate, and the rule
// is reversible, so the test never hard-codes an id the bridge could not resolve.
const REF: ResourceRef = { owner: NODE, id: resourceIdOf(PLATFORM, CONVERSATION) };

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
let node: OnlyneNode;
let adoptions = 0;
/** The host's adopt: one resource per conversation, bound to the assignee (RFC 0012 §2). */
const adopt = async (ref: ResourceRef, conversation: string): Promise<void> => {
  adoptions += 1;
  await node.resources.register({ actor: NODE } satisfies Auth, {
    resource_ref: ref,
    kind: 'im_conversation',
    operations: ['read', 'send', 'list_events', 'handover', 'describe'],
    binding: { handler: ASSIGNEE, cursor: '0' },
    log_readers: [ASSIGNEE],
    revision: 'rev-a',
    event_seq: '0',
    retention_ms: 3_600_000,
    data: { platform: PLATFORM, conversation },
  });
};
// The bridge resolves the resource service lazily: the library captures the driver when the
// service is constructed (resources.ts:159), so the host builds the bridge first.
const bridge = createDriverBridge({
  node: NODE,
  resources: () => node.resources,
  storage: { transaction: work => storage.transaction(work) },
  connectors: [{ platform: PLATFORM, connector }],
  adopt,
  writeAttachment: async (_ref, name, bytes) => {
    stored.push({ name, text: new TextDecoder().decode(bytes) });
    return `/tmp/${name}`;
  },
});
node = new OnlyneNode({
  node_id: NODE,
  base_url: 'http://127.0.0.1:3999/onlyne/v3',
  plugins: { ...plugins, storage, resources: bridge },
  now: () => Date.now(),
  id: () => Math.random().toString(36).slice(2),
});

ok('the resource id round-trips through the coordinate rule',
  REF.id === 'lark-oc_test'
  && coordinateOf(REF.id)?.platform === PLATFORM && coordinateOf(REF.id)?.conversation === CONVERSATION
  && coordinateOf('chat_01') === undefined);

const event = (id: string, text: string, conversation: string = CONVERSATION): ImEvent => ({
  platform: PLATFORM, source_id: id, conversation, kind: 'message',
  from: 'ou_sender', text, attachments: [], message_handle: `om_${id}`, at: '2026-10-26T12:00:00Z',
});

await bridge.receive(event('p-1', '第一条'));
const resource = await node.resources.describe({ actor: ASSIGNEE }, REF);
const resourceData: unknown = resource.data;
const adoptedConversation = resourceData !== null && typeof resourceData === 'object' && 'conversation' in resourceData
  ? resourceData.conversation
  : undefined;
ok('the first event adopts the resource with its binding and log reader',
  resource.binding.handler === ASSIGNEE && resource.log_readers.includes(ASSIGNEE)
  && adoptedConversation === CONVERSATION);
const first = await node.resources.log({ actor: ASSIGNEE }, REF, {});
ok('one event lands', first.items.length === 1 && first.items[0]!.source_id === 'p-1');

await bridge.receive(event('p-1', '第一条'));
const afterDuplicate = await node.resources.log({ actor: ASSIGNEE }, REF, {});
ok('duplicate source_id produces no second event', afterDuplicate.items.length === 1);
ok('a repeated first touch reuses the adopted resource', adoptions === 1);

const sameRef = await bridge.resolve(PLATFORM, CONVERSATION);
ok('resolving a known conversation answers the stored resource', sameRef.id === REF.id && adoptions === 1);

await bridge.receive(event('p-2', '回复目标'));
const second = await node.resources.log({ actor: ASSIGNEE }, REF, {});
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
ok('the send target is the resource\'s own coordinate', connector.sends[0]?.conversation === CONVERSATION);

// RFC 0012 §5: `reply_to_event` names an event of the operated resource. Event ids are
// resource-local, so an id another resource holds is a caller error, not a missing event.
const other = await bridge.resolve(PLATFORM, 'oc_other');
const crossed = await node.resources.operate(assignee, other, {
  operation_id: 'op-cross', operation: 'send',
  data: { text: '跨资源引用', reply_to_event: secondId },
  ...mutation(),
});
ok('a reply naming another resource\'s event is refused', crossed.state === 'aborted'
  && crossed.error?.code === 'invalid_request');
await bridge.receive(event('q-1', '别的会话', 'oc_other'));
connector.answers.push({ state: 'committed', message_id: 'om_other_local' });
const localReply = await node.resources.operate(assignee, other, {
  operation_id: 'op-local', operation: 'send',
  data: { text: '本资源引用', reply_to_event: '1' },
  ...mutation(),
});
ok('the other resource quotes its own first event', localReply.state === 'committed'
  && connector.sends.at(-1)?.conversation === 'oc_other' && connector.sends.at(-1)?.reply_to === 'om_q-1');

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

const sendsBeforeEmpty = connector.sends.length;
try {
  await node.resources.operate(assignee, REF, {
    operation_id: 'op-4', operation: 'send',
    data: { text: '' },
    ...mutation(),
  });
  ok('empty text is rejected before the connector', false);
} catch (error) {
  ok('empty text is rejected before the connector', (error as { error?: { code?: string } }).error?.code === 'invalid_request');
  ok('the connector saw no send for the empty text', connector.sends.length === sendsBeforeEmpty);
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
  connectors: [{ platform: PLATFORM, connector }],
  adopt,
});
const unhooked = await hookless.execute(resource, {
  operation_id: 'op-10', operation: 'send', ...mutation(),
  data: { attachments: [{ kind: 'file', name: 'a.txt', content: 'eA==' }] },
});
ok('inline content without a host hook is refused', unhooked.state === 'aborted'
  && unhooked.error?.code === 'unsupported_delivery');

// The handler's cursor is the delivery position (RFC 0006 §5): an ack moves it forward, an ack
// naming an event already behind it leaves it alone, and no other node may acknowledge.
await bridge.receive(event('p-3', '积压'));
const events = await node.resources.log(assignee, REF, {});
ok('the log lists every event in id order', events.items.map(item => item.event_id).join(',') === '1,2,3');
const acked = await node.resources.ack(assignee, REF, '3', mutation());
ok('an ack advances the handler cursor', acked.cursor === '3');
const repeated = await node.resources.ack(assignee, REF, '1', mutation());
ok('an ack behind the cursor leaves it alone', repeated.cursor === '3');
let foreignAck = false;
try {
  await node.resources.ack({ actor: 'urn:onlyne:agent:next' }, REF, '3', mutation());
} catch (error) {
  foreignAck = (error as { error?: { code?: string } }).error?.code === 'forbidden';
}
ok('only the handler acknowledges events', foreignAck);

// The target side of a handover (RFC 0006 §4.3): a probe is answered once per operation id, and
// only the node that sent it may read the answer back.
const NEXT = 'urn:onlyne:agent:next';
const probe = {
  operation_id: 'probe-1', resource: REF, kind: 'im_conversation',
  operations: ['read', 'send', 'handover'], initiator: NEXT,
  cursor: '3', deadline: deadline(),
};
const answer = await node.resources.probe({ actor: NEXT }, probe);
ok('the default probe policy accepts at the offered cursor', answer.answer === 'accept' && answer.cursor === '3');
const replayed = await node.resources.probe({ actor: NEXT }, probe);
ok('a replayed probe returns the recorded answer', JSON.stringify(replayed) === JSON.stringify(answer));
const recorded = await node.resources.probeAnswer({ actor: NEXT }, 'probe-1');
ok('the initiator reads its recorded answer back', recorded.answer === 'accept');
let foreignProbe = false;
try {
  await node.resources.probeAnswer({ actor: 'urn:onlyne:agent:stranger' }, 'probe-1');
} catch (error) {
  foreignProbe = (error as { error?: { code?: string } }).error?.code === 'not_found';
}
ok('a probe answer is readable only by its initiator', foreignProbe);

// The initiator side: a target this node cannot reach aborts the handover and leaves the binding alone.
const initiated = await node.resources.operate(assignee, REF, {
  operation_id: 'op-5', operation: 'handover',
  data: { target: NEXT, handover_deadline: deadline() },
  ...mutation(),
});
ok('an unreachable target aborts the handover', initiated.state === 'aborted'
  && (initiated.data as Record<string, string> | undefined)?.reason === 'unreachable');
const unchanged = await node.resources.describe(assignee, REF);
ok('an aborted handover leaves the handler and cursor alone',
  unchanged.binding.handler === ASSIGNEE && unchanged.binding.cursor === '3');

if (failures > 0) {
  console.error(`${failures} failure(s)`);
  process.exit(1);
}
console.log('im-core verify: all checks passed');
