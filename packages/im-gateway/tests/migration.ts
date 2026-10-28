import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import {
  MemoryStorage, OnlyneError, OnlyneNode,
  type Storage, type ThreadRef, type TraceEvent, type Transport,
} from '@onlyne/lib';
import type { Connector, ImEvent, SendOutcome, SendRequest } from '@onlyne/im-core';
import { createImGateway, nodeClock, type GatewayConfig, type ImGateway } from '../src/index.js';
import { applyMigration, inspectStore, planMigration, rollbackMigration } from '../src/migrate.js';

/**
 * Offline migration round trip. A legacy aggregate store (`urn:onlyne:gateway:im`) holds one lark
 * conversation with a binding cursor, a rendered reply, an in-flight render and an undelivered
 * push. The test plans and applies the move to `urn:onlyne:gateway:im-lark`, then proves that the
 * new owner path receives, sends, renders and delivers the preserved push identity, and that a
 * rollback restores the old owner path byte for byte.
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
const rec = (value: unknown): Record<string, unknown> | undefined =>
  value !== null && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : undefined;
const rows = (value: unknown): Array<Record<string, unknown> | undefined> =>
  Array.isArray(value) ? value.map(rec) : [];

const OLD = 'urn:onlyne:gateway:im';
const NEW = 'urn:onlyne:gateway:im-lark';
const AGENT = 'urn:onlyne:agent:lazuli';
const CONVERSATION = 'oc_mig';
const RESOURCE = `lark-${CONVERSATION}`;
const THREAD = `im-${RESOURCE}`;
const NOTIFY_3 = `im-notify-${OLD}-${RESOURCE}-3`;

class ScriptedConnector implements Connector {
  readonly platform = 'lark';
  readonly sends: SendRequest[] = [];
  failOn?: string;
  #listeners: Array<(event: ImEvent) => void> = [];
  async start(): Promise<void> {}
  onEvent(listener: (event: ImEvent) => void): void { this.#listeners.push(listener); }
  async send(request: SendRequest): Promise<SendOutcome> {
    if (this.failOn !== undefined && request.text === this.failOn) throw new Error('platform transport failed');
    this.sends.push(request);
    return { state: 'committed', message_id: `om_sent_${this.sends.length}` };
  }
  async stop(): Promise<void> { this.#listeners.length = 0; }
}

const event = (source: string): ImEvent => ({
  platform: 'lark', source_id: source, conversation: CONVERSATION, kind: 'message',
  from: 'ou_sender', text: `消息 ${source}`, attachments: [], message_handle: `fk_${source}`,
  at: new Date().toISOString(),
});

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

let reachable = true;
const transportFor = (actor: string, node: OnlyneNode = agent): Transport => ({
  async request<T>(target: string, method: string, path: string, body?: unknown): Promise<T> {
    if (!reachable) throw new OnlyneError('unavailable', 'The test agent is offline.');
    if (target !== AGENT) throw new OnlyneError('unavailable', `The test routes no peer ${target}.`);
    const response = await node.fetch(new Request(`http://agent.test/onlyne/v3${path}`, {
      method,
      headers: { 'content-type': 'application/json', 'Onlyne-Actor': actor },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    }));
    const text = await response.text();
    if (!response.ok) {
      const failure = rec(text === '' ? {} : JSON.parse(text));
      const error = rec(failure?.error);
      throw new OnlyneError(
        (typeof error?.code === 'string' ? error.code : 'unavailable') as 'unavailable',
        typeof error?.message === 'string' ? error.message : 'The test agent refused.',
      );
    }
    return (text === '' ? undefined : JSON.parse(text)) as T;
  },
});

const dir = await mkdtemp(join(tmpdir(), 'im-migration-'));
const storePath = join(dir, 'store.json');
const snapshotPath = join(dir, 'snapshot.json');
const connector = new ScriptedConnector();
const oldConfig: GatewayConfig = {
  node: { id: OLD },
  server: { bind: '127.0.0.1', port: 3997 },
  storage_path: storePath,
  channel: { platform: 'lark', conversation: CONVERSATION, assignee: AGENT },
};
const newConfig: GatewayConfig = {
  node: { id: NEW },
  server: { bind: '127.0.0.1', port: 3998 },
  storage_path: storePath,
  channel: { platform: 'lark', conversation: CONVERSATION, assignee: AGENT },
};
const factories = { lark: async () => connector };
const mutation = () => ({ idempotency_key: crypto.randomUUID(), retry_until: new Date(Date.now() + 60_000).toISOString() });
const call = async (gateway: ImGateway, method: string, path: string, body?: unknown): Promise<{ status: number; body: Record<string, unknown> }> => {
  const response = await gateway.node.fetch(new Request(`http://127.0.0.1:${gateway.config.server.port}/onlyne/v3${path}`, {
    method,
    headers: { 'content-type': 'application/json', 'Onlyne-Actor': AGENT },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  }));
  const text = await response.text();
  return { status: response.status, body: (text === '' ? {} : JSON.parse(text)) as Record<string, unknown> };
};
const reply = async (gateway: ImGateway, id: string, msg: string): Promise<void> => {
  const sent = await call(gateway, 'POST', `/threads/${gateway.config.node.id}/${THREAD}/messages`, {
    ...mutation(),
    message: { id, sender: AGENT, delivery_kind: 'reply', msg },
    target_endpoint: gateway.config.node.id,
    plan: { response: 'none', execution: 'existing' },
  });
  if (sent.status !== 201) throw new Error(`the test reply did not land: ${sent.status} ${JSON.stringify(sent.body)}`);
};

try {
  const old = await createImGateway(oldConfig, { storage: 'disk', factories, transport: transportFor(OLD), log: () => undefined });
  await settle();
  await old.receive(event('m-1'));
  await old.receive(event('m-2'));
  const ack = await call(old, 'POST', `/resources/${OLD}/${RESOURCE}/events/1/ack`, mutation());
  ok('the legacy conversation has a binding cursor', ack.status === 200 && ack.body.cursor === '1');
  await reply(old, 'agent-reply-1', '第一条回复');
  await old.maintain();
  ok('the legacy renderer sent the first reply', connector.sends.length === 1);

  // An interrupted render leaves its safety mark behind; migration must carry it, not resend it.
  connector.failOn = '第二条回复';
  await reply(old, 'agent-reply-2', '第二条回复');
  await old.maintain();
  connector.failOn = undefined;
  const rendering = await old.storage.transaction(async tx =>
    (await tx.scan<{ state: string }>(`im/render/${encodeURIComponent(OLD)}/${RESOURCE}/seq/`))
      .filter(row => row.value.state === 'rendering'));
  ok('an interrupted render leaves one in-flight mark', rendering.length === 1);

  // An undelivered push: the agent is offline when the third event arrives.
  reachable = false;
  await old.receive(event('m-3'));
  await settle();
  const owed = await old.storage.transaction(async tx => tx.scan('im/push/'));
  ok('an offline recipient leaves the third push owed', owed.length === 1);
  await old.stop();
  // A row written before the projection id was persisted: migration must reconstruct the id the
  // old endpoint would have used, not mint one under the new identity.
  await old.storage.transaction(async tx => {
    for (const row of await tx.scan<Record<string, unknown>>('im/push/')) {
      const { message_id: _dropped, ...rest } = row.value;
      await tx.put(row.key, rest);
    }
  });

  const original = await readFile(storePath, 'utf8');
  const pairing = { node_id: NEW };
  const plan = await planMigration({ store_path: storePath, from: OLD, to: NEW, platform: 'lark', pairing, snapshot_path: snapshotPath, now: () => Date.now() });
  ok('the plan moves the one resource and its thread', plan.report.resources.length === 1
    && plan.report.resources[0]?.from.owner === OLD && plan.report.resources[0]?.to.owner === NEW
    && plan.report.resources[0]?.thread.from.owner === OLD && plan.report.resources[0]?.thread.to.owner === NEW);
  ok('the plan reports the in-flight render and the owed push', plan.report.rendering_in_flight === 1
    && plan.report.pushes === 1 && plan.report.reattributed_messages >= 2);
  ok('the plan keeps the resource id', plan.report.resources[0]?.to.id === RESOURCE);

  const missingPair = await planMigration({ store_path: storePath, from: OLD, to: NEW, platform: 'lark', pairing: { node_id: 'urn:onlyne:gateway:im-qq' } })
    .then(() => undefined, error => error);
  ok('a migration without the new identity pairing is refused', missingPair instanceof OnlyneError);
  const wrongPlatform = await planMigration({ store_path: storePath, from: OLD, to: 'urn:onlyne:gateway:im-qq', platform: 'lark', pairing: { node_id: 'urn:onlyne:gateway:im-qq' } })
    .then(() => undefined, error => error);
  ok('an identity from another platform is refused', wrongPlatform instanceof OnlyneError);

  const report = await applyMigration({ store_path: storePath, from: OLD, to: NEW, platform: 'lark', pairing, snapshot_path: snapshotPath });
  ok('the snapshot is byte-identical to the pre-migration store', await readFile(snapshotPath, 'utf8') === original);
  ok('the report is written beside the store', (await readFile(report.report_path, 'utf8')).includes(NEW));
  const migrated = JSON.parse(await readFile(storePath, 'utf8')) as Record<string, unknown>;
  /** The part of a store key that names an owner; a preserved message id may still spell the old one. */
  const ownerPart = (key: string): string | undefined => {
    const parts = key.split('/').map(decodeURIComponent);
    switch (parts[0]) {
      case 'resource': case 'thread': case 'entry': case 'event': case 'member': case 'grant':
      case 'known': case 'attention': case 'default': case 'message': case 'entry-ref': case 'outbound':
        return parts[1];
      case 'consumed': case 'todo':
        return parts[2];
      case 'inbox': {
        const obligation = (parts[1] ?? '').split('/').map(decodeURIComponent);
        return obligation[0];
      }
      case 'im':
        return parts[1] === 'handle' || parts[1] === 'push' || parts[1] === 'notify' || parts[1] === 'render'
          ? parts[2]
          : undefined;
      default:
        return undefined;
    }
  };
  ok('no store row is owned by the old identity any more', Object.keys(migrated).every(key => ownerPart(key) !== OLD)
    && Object.keys(migrated).some(key => key.startsWith(`resource/${encodeURIComponent(NEW)}/${RESOURCE}`)));
  const inspected = await inspectStore(storePath);
  ok('the inspection reports only the new identity', inspected.resources.length === 1
    && inspected.resources[0]?.owner === NEW && inspected.threads[0]?.owner === NEW);

  const moved = await createImGateway(newConfig, { storage: 'disk', factories, transport: transportFor(NEW), log: () => undefined });
  await settle();
  const described = await call(moved, 'GET', `/resources/${NEW}/${RESOURCE}`);
  ok('the new owner path serves the migrated resource', described.status === 200
    && rec(described.body.binding)?.handler === AGENT && rec(described.body.binding)?.cursor === '1');
  const log = await call(moved, 'GET', `/resources/${NEW}/${RESOURCE}/log`);
  ok('the migrated event log keeps its source ids and order', rows(log.body.items).map(row => row?.source_id).join(',') === 'm-1,m-2,m-3');
  const thread = await moved.node.messages.list(moved.node.auth, { owner: NEW, id: THREAD }, { attention: 'thread' });
  ok('historical projections are re-attributed to the new endpoint', thread.items[0]?.message.sender === NEW);
  const inFlight = await moved.storage.transaction(async tx => tx.get<{ state: string }>(`im/render/${encodeURIComponent(NEW)}/${RESOURCE}/seq/4`));
  ok('the in-flight render mark moved with its owner', inFlight?.state === 'rendering');

  reachable = true;
  await moved.maintain();
  const delivered = await moved.node.messages.list(moved.node.auth, { owner: NEW, id: THREAD }, { attention: 'thread' });
  ok('the owed push keeps the identity the old endpoint would have sent', delivered.items.some(entry => entry.message.id === NOTIFY_3));

  const send = await call(moved, 'POST', `/resources/${NEW}/${RESOURCE}/operations`, {
    ...mutation(), operation_id: 'send-after-move', operation: 'send', data: { text: '迁移后发送' },
  });
  ok('a send through the new owner path commits', send.status === 201 && send.body.state === 'committed'
    && connector.sends.at(-1)?.text === '迁移后发送');
  await moved.receive(event('m-4'));
  const afterReceive = await call(moved, 'GET', `/resources/${NEW}/${RESOURCE}/log`);
  ok('a receive through the new owner path appends the next event', rows(afterReceive.body.items).length === 4
    && rows(afterReceive.body.items).at(-1)?.source_id === 'm-4');
  await reply(moved, 'agent-reply-3', '第三条回复');
  await moved.maintain();
  ok('the new owner path renders only the new reply', connector.sends.map(request => request.text).join(',') === '第一条回复,迁移后发送,第三条回复');
  await moved.stop();

  const rolledBack = await rollbackMigration({ store_path: storePath, snapshot_path: snapshotPath });
  ok('rollback restores the store byte for byte', await readFile(storePath, 'utf8') === original);
  ok('rollback keeps the migrated store as a safety copy', await readFile(rolledBack.safety_path, 'utf8') !== original);
  const restored = await inspectStore(storePath);
  ok('the rolled-back store belongs to the old identity', restored.resources[0]?.owner === OLD);

  const oldAgain = await createImGateway(oldConfig, { storage: 'disk', factories, transport: transportFor(OLD), log: () => undefined });
  await settle();
  const oldPath = await call(oldAgain, 'GET', `/resources/${OLD}/${RESOURCE}`);
  ok('the old owner path serves the restored resource', oldPath.status === 200
    && rec(oldPath.body.binding)?.handler === AGENT && rec(oldPath.body.binding)?.cursor === '1');
  const oldLog = await call(oldAgain, 'GET', `/resources/${OLD}/${RESOURCE}/log`);
  ok('the restored log keeps its three events', rows(oldLog.body.items).length === 3);
  const oldSend = await call(oldAgain, 'POST', `/resources/${OLD}/${RESOURCE}/operations`, {
    ...mutation(), operation_id: 'send-after-rollback', operation: 'send', data: { text: '回滚后发送' },
  });
  ok('a send through the restored old owner path commits', oldSend.status === 201
    && connector.sends.at(-1)?.text === '回滚后发送');
  await oldAgain.stop();

  // A pending handover is refused instead of being rewritten under a dead probe reference.
  const pendingPath = join(dir, 'pending.json');
  const pending = JSON.parse(original) as Record<string, unknown>;
  pending[`resource/${encodeURIComponent(OLD)}/${RESOURCE}/handover/h-1`] = {
    operation_id: 'h-1', resource_ref: { owner: OLD, id: RESOURCE }, initiator: AGENT,
    target: AGENT, kind: 'im_conversation', operations: ['send'], expected_revision: 'rev-1',
    deadline: new Date(Date.now() + 60_000).toISOString(), cursor: '1', digest: 'x', state: 'open',
  };
  await writeFile(pendingPath, JSON.stringify(pending));
  const pendingError = await planMigration({ store_path: pendingPath, from: OLD, to: NEW, platform: 'lark', pairing })
    .then(() => undefined, error => error);
  ok('an open handover refuses the migration', pendingError instanceof OnlyneError
    && String(pendingError.message).includes('handover'));

  // ---- owner rename: the peer must learn the new ThreadRef and drop the dead one -----------
  // The reporter's state. A grant was accepted and acked under the old owner, so the peer holds
  // a `known` row naming the endpoint the migration kills, the member row keeps `#ensureMember`
  // from issuing another grant, and the notice stays acked. Nothing in that state re-teaches the
  // peer, so every attention the new owner produces afterwards waits forever.
  const renameStore = join(dir, 'rename-store.json');
  const RENAME_CONVERSATION = 'oc_ren';
  const RENAME_RESOURCE = `lark-${RENAME_CONVERSATION}`;
  const RENAME_THREAD = `im-${RENAME_RESOURCE}`;
  const renamed = (source: string): ImEvent => ({ ...event(source), conversation: RENAME_CONVERSATION, text: `改名 ${source}` });
  let peerNow = Date.now();
  const peerStore = new MemoryStorage();
  const peerTraces: TraceEvent[] = [];
  /** The endpoints the peer can reach; the old owner's route disappears with the migration. */
  const peerRoutes = new Map<string, ImGateway>();
  const peerTransport: Transport = {
    async request<T>(target: string, method: string, path: string, body?: unknown): Promise<T> {
      const gateway = peerRoutes.get(target);
      if (gateway === undefined) throw new OnlyneError('unavailable', `The peer has no route to ${target}.`);
      const response = await gateway.node.fetch(new Request(`http://peer.test/onlyne/v3${path}`, {
        method,
        headers: { 'content-type': 'application/json', 'Onlyne-Actor': AGENT },
        ...(body === undefined ? {} : { body: JSON.stringify(body) }),
      }));
      const text = await response.text();
      if (!response.ok) {
        const failure = rec(text === '' ? {} : JSON.parse(text));
        const error = rec(failure?.error);
        throw new OnlyneError(
          (typeof error?.code === 'string' ? error.code : 'unavailable') as 'unavailable',
          typeof error?.message === 'string' ? error.message : 'The peer gateway refused.',
        );
      }
      return (text === '' ? undefined : JSON.parse(text)) as T;
    },
  };
  const peer = new OnlyneNode({
    node_id: AGENT,
    base_url: 'http://peer.test/onlyne/v3',
    plugins: {
      storage: peerStore,
      authenticate: async request => {
        const actor = request.headers.get('Onlyne-Actor');
        return actor === null || actor.length === 0 ? null : { actor };
      },
      resolve: async () => { throw new OnlyneError('not_found', 'The test peer resolves nobody.'); },
      decide: () => { throw new OnlyneError('decision_unavailable', 'The test peer decides nothing.'); },
      transport: peerTransport,
      maintenanceClock: nodeClock(),
      runtime: {
        notify: () => undefined,
        supports: async () => false,
        inject: async (_entry, context) => ({ context: context ?? 'peer', consumed: true, outcome: 'delivered' }),
        trace: event => { peerTraces.push(event); },
      },
    },
    now: () => peerNow,
    id: () => crypto.randomUUID(),
    // The dedup window is proved by moving this clock, so its own retry ceiling stays short
    // enough that a mutation still lands inside the gateway's default 24-hour window.
    retry_max_ms: 60_000,
  });
  const renameConnector = new ScriptedConnector();
  const renameFactories = { lark: async () => renameConnector };
  const oldRenameConfig: GatewayConfig = { ...oldConfig, storage_path: renameStore, server: { bind: '127.0.0.1', port: 4101 } };
  const newRenameConfig: GatewayConfig = { ...newConfig, storage_path: renameStore, server: { bind: '127.0.0.1', port: 4102 } };
  const rowsOf = async <T>(storage: Storage, prefix: string): Promise<T[]> =>
    storage.transaction(async tx => (await tx.scan<T>(prefix)).map(row => row.value));
  const isOldOwnerFailure = (event: TraceEvent): event is Extract<TraceEvent, { event: 'reconcile.failed' }> =>
    event.event === 'reconcile.failed' && event.owner === OLD;

  const renameOld = await createImGateway(oldRenameConfig, { storage: 'disk', factories: renameFactories, transport: transportFor(OLD, peer), log: () => undefined });
  await settle();
  peerRoutes.set(OLD, renameOld);
  await renameOld.receive(renamed('r-1'));
  await settle();
  // The peer accepts the pushed notice and confirms it: the owner's notice is closed, and the
  // peer keeps the only record that teaches it where the thread lives.
  await peer.client.maintain();
  const staleKnown = await rowsOf<{ thread_ref: ThreadRef }>(peerStore, 'known/');
  ok('the peer learned the thread under the old owner', staleKnown.length === 1
    && staleKnown[0]?.thread_ref.owner === OLD && staleKnown[0]?.thread_ref.id === RENAME_THREAD);
  const ackedNotice = await rowsOf<Record<string, unknown>>(renameOld.storage, 'notice/');
  ok('the old grant is acked before the migration', ackedNotice.length === 1 && ackedNotice[0]?.acked === true);
  await renameOld.stop();

  const renamePlan = await planMigration({
    store_path: renameStore, from: OLD, to: NEW, platform: 'lark',
    pairing: { node_id: NEW }, snapshot_path: join(dir, 'rename-snapshot.json'), now: () => Date.now(),
  });
  ok('the plan re-issues the notice the member already acked', renamePlan.report.notices_reissued === 1);
  await applyMigration({
    store_path: renameStore, from: OLD, to: NEW, platform: 'lark',
    pairing: { node_id: NEW }, snapshot_path: join(dir, 'rename-snapshot.json'),
  });
  peerRoutes.delete(OLD);

  // The dead owner is reported instead of skipped, once per window however many passes fail.
  await peer.client.reconcile();
  await peer.client.reconcile();
  await peer.client.reconcile();
  const firstFailures = peerTraces.filter(isOldOwnerFailure);
  ok('a dead old owner reports one deduplicated reconcile failure',
    firstFailures.length === 1 && firstFailures[0]?.code === 'unavailable'
      && firstFailures[0]?.thread === RENAME_THREAD && firstFailures[0]?.attempts === 1);
  // The window closes: the next line accounts for the passes the first one folded away.
  peerNow += 6 * 60_000;
  await peer.client.reconcile();
  const windowedFailures = peerTraces.filter(isOldOwnerFailure);
  ok('the closed window reports the attempts it suppressed', windowedFailures.length === 2
    && windowedFailures[1]?.attempts === 3);

  const renameNew = await createImGateway(newRenameConfig, { storage: 'disk', factories: renameFactories, transport: transportFor(NEW, peer), log: () => undefined });
  await settle();
  peerRoutes.set(NEW, renameNew);
  // The new owner's maintenance pass carries the re-issued notice with no caller change.
  await renameNew.node.client.maintain();
  const replacedKnown = await rowsOf<{ thread_ref: ThreadRef }>(peerStore, 'known/');
  ok('the peer holds the thread under the new owner only', replacedKnown.length === 1
    && replacedKnown[0]?.thread_ref.owner === NEW && replacedKnown[0]?.thread_ref.id === RENAME_THREAD);
  await peer.client.maintain();
  const closedNotice = await rowsOf<Record<string, unknown>>(renameNew.storage, 'notice/');
  ok('the re-issued notice is closed by the peer again', closedNotice.length === 1
    && closedNotice[0]?.acked === true
    && rec(closedNotice[0]?.thread_ref)?.owner === NEW
    && rec(closedNotice[0]?.previous_thread_ref)?.owner === OLD);

  // The next event's attention is what the peer could never reach before: it now pulls it.
  await renameNew.receive(renamed('r-2'));
  await settle();
  const beforePull = await rowsOf<{ acked: boolean }>(renameNew.storage, 'attention/');
  ok('the new owner queues attention for the peer', beforePull.some(row => row.acked === false));
  await peer.client.maintain();
  const afterPull = await rowsOf<{ acked: boolean }>(renameNew.storage, 'attention/');
  ok('the peer pulls and acks the attention once it knows the new owner',
    afterPull.length >= 2 && afterPull.every(row => row.acked === true));
  await renameNew.stop();
} finally {
  await rm(dir, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`im-gateway migration: ${failures} check(s) failed`);
  process.exit(1);
}
console.log('im-gateway migration: all checks passed');
