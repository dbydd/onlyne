import { OnlyneNode, type Auth, type Plugins, type ResourceRef } from '@onlyne/lib';
import { MemoryStorage } from '@onlyne/lib/memory';
import { createDriverBridge, resourceIdOf, type ImEvent, type SendOutcome } from '@onlyne/im-core';
import { createFakeConnector } from '../src/index.js';

let failures = 0;
const ok = (label: string, condition: boolean) => {
  if (condition) console.log(`ok   ${label}`);
  else { failures += 1; console.error(`FAIL ${label}`); }
};

const NODE = 'urn:onlyne:gateway:im';
const ASSIGNEE = 'urn:onlyne:agent:lazuli';
const PLATFORM = 'fake';
const CONVERSATION = 'fake_conv';
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

const answers: SendOutcome[] = [];
const connector = createFakeConnector({
  events: [
    event('p-1', '第一条'), event('p-1', '第一条（重复）'),
  ],
  answers,
});

function event(id: string, text: string): ImEvent {
  return {
    platform: PLATFORM, source_id: id, conversation: CONVERSATION, kind: 'message',
    from: 'fake_user', text, attachments: [],
    at: '2026-10-26T12:00:00Z',
  };
}

const storage = new MemoryStorage();
let node: OnlyneNode;
/** The host's adopt: the conversation's resource, created when the connector's first event lands. */
const adopt = async (ref: ResourceRef, conversation: string): Promise<void> => {
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
});
node = new OnlyneNode({
  node_id: NODE,
  base_url: 'http://127.0.0.1:3999/onlyne/v3',
  plugins: { ...plugins, storage, resources: bridge },
  now: () => Date.now(),
  id: () => Math.random().toString(36).slice(2),
});

const assignee: Auth = { actor: ASSIGNEE };
const mutation = () => ({
  idempotency_key: 'm-' + Math.random().toString(36).slice(2),
  retry_until: new Date(Date.now() + 3_600_000).toISOString(),
});

// The connector replays its script into the bridge, which is the inbound path a host wires.
connector.onEvent(replayed => { void bridge.receive(replayed); });
await connector.start();
const tick = Promise.withResolvers<void>();
setTimeout(tick.resolve, 0);
await tick.promise;
const events = await node.resources.log(assignee, REF, {});
ok('two same source_id events produce one event', events.items.length === 1 && events.items[0]!.source_id === 'p-1');

answers.push({ state: 'committed', message_id: 'om_fake_out_1', ref_handle: 'om_fake_out_1' });
const committed = await node.resources.operate(assignee, REF, {
  operation_id: 'op-1', operation: 'send',
  data: { text: '你好', reply_to_event: events.items[0]!.event_id },
  ...mutation(),
});
ok('send commits with the platform message id', committed.state === 'committed'
  && committed.data !== null && typeof committed.data === 'object'
  && 'message_id' in committed.data && committed.data.message_id === 'om_fake_out_1');

let threw = false;
try {
  await connector.send({
    conversation: CONVERSATION, text: '第二条', format: 'plain', attachments: [],
  });
} catch {
  threw = true;
}
ok('an empty answers queue throws', threw);

if (failures > 0) process.exit(1);
console.log('im-fake verify: all checks passed');
