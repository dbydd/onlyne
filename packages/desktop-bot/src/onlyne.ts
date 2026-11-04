import { mkdir } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import {
  OnlyneError, OnlyneNode, trustPhase,
  type Action, type Auth, type DeliveryPlan, type Entry, type Json, type KeyExchange,
  type KeyHook, type KeyMaterial, type LocalAttention, type MaintenanceClock,
  type MessageRef, type MsgInput, type NodeId, type NotifyEvent, type Page,
  type Plugins, type ResourceRef, type Runtime, type SendRequest, type ThreadRef,
  type SendOutcome, type TraceEvent, type TrustEntry, type TrustPhase,
} from '@onlyne/lib';
import { ensureKeyDir, loadOrCreateKey, writeKeyMaterial } from '@onlyne/node-host/keys';
import { createLogger, startNodeHost } from '@onlyne/node-host';
import { FileStorage } from '@onlyne/im-gateway/storage';
import type { DesktopEvent, HostToolContext, OnlyneBridge, OnlyneBridgeOptions } from './contracts.ts';

const BIND = '127.0.0.1';
const SERVICE_PATH = '/onlyne/v3';
const PAGE = 100;
const MAX_PAGES = 100;
const MAX_SEEN = 4096;
const RESPONSES = ['none', 'async', 'await'] as const;
const EXECUTIONS = ['existing', 'new'] as const;
const READ_SCOPES = ['directory', 'neighbors', 'threads', 'inbox', 'messages', 'message', 'evidence', 'resources'] as const;
const THREAD_OPS = ['create', 'close', 'list', 'describe', 'member_add', 'member_remove'] as const;
const DOMAIN_OPS = ['create', 'join', 'leave', 'list', 'describe', 'discover'] as const;

export interface PeerRow {
  nodeId: string;
  baseUrl: string;
  fingerprint: string | null;
  phase: TrustPhase | 'no_exchange';
  state: TrustEntry['state'] | null;
  localApproved: boolean;
  remoteApproved: boolean;
  verified: boolean;
  lastSeen: string | null;
  source: string | null;
  instanceId: string | null;
  firstSeen: string | null;
  lastChecked: string | null;
}

export interface PendingExchangeRow {
  nodeId: string;
  fingerprint: string;
  baseUrl: string;
  at: string;
}

export interface DesktopOnlyneStatus {
  node: { id: string; endpoint: string; fingerprint: string; instanceId: string; authentication: string[]; state: 'ready' | 'closed' };
  trust: { localFingerprint: string; peers: PeerRow[]; pending: PendingExchangeRow[] };
  neighbors: PeerRow[];
  threads: unknown[];
  joined: unknown[];
  resources: { owners: Array<{ owner: string; count: number; items: unknown[]; truncated: boolean }> };
  contexts: Array<{ alias: string; summary: string; state: string }>;
  inbox: { pending: number };
}

function invalid(message: string): never { throw new OnlyneError('invalid_request', message); }

function text(value: unknown, name: string): string {
  if (typeof value !== 'string' || value.length === 0) invalid(`${name} must be a non-empty string.`);
  return value;
}

function oneOf<T extends string>(value: unknown, allowed: readonly T[], name: string): T {
  const found = typeof value === 'string' ? allowed.find(item => item === value) : undefined;
  if (found === undefined) invalid(`${name} must be one of ${allowed.join(', ')}.`);
  return found;
}

function objectOf(value: unknown, name: string): Record<string, unknown> {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) invalid(`${name} must be a JSON object.`);
  return value as Record<string, unknown>;
}

function refOf(value: unknown, owner: 'owner' | 'sender', name: string): { owner?: string; sender?: string; id: string } {
  const object = objectOf(value, name);
  return { [owner]: text(object[owner], `${name}.${owner}`), id: text(object.id, `${name}.id`) };
}

function threadRefOf(value: unknown, name: string): ThreadRef {
  return refOf(value, 'owner', name) as ThreadRef;
}

function messageRefOf(value: unknown, name: string): MessageRef {
  return refOf(value, 'sender', name) as MessageRef;
}

function resourceRefOf(value: unknown, name: string): ResourceRef {
  return refOf(value, 'owner', name) as ResourceRef;
}

function jsonValue(value: unknown, name: string): Json {
  if (value === null || typeof value === 'boolean' || typeof value === 'string') return value;
  if (typeof value === 'number' && Number.isFinite(value)) return value;
  if (Array.isArray(value)) return value.map((item, index) => jsonValue(item, `${name}[${index}]`));
  if (typeof value === 'object') {
    const result: { [key: string]: Json } = {};
    for (const [key, item] of Object.entries(value)) result[key] = jsonValue(item, `${name}.${key}`);
    return result;
  }
  return invalid(`${name} is not JSON.`);
}

function jsonRecord(value: Record<string, unknown>, name: string): Record<string, Json> {
  const result: Record<string, Json> = {};
  for (const [key, item] of Object.entries(value)) {
    if (item !== undefined) result[key] = jsonValue(item, `${name}.${key}`);
  }
  return result;
}

function numberField(value: unknown, name: string): number | undefined {
  if (value === undefined) return undefined;
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value <= 0) invalid(`${name} must be a positive integer.`);
  return value;
}

function stringList(value: unknown, name: string): string[] | undefined {
  if (value === undefined) return undefined;
  if (!Array.isArray(value)) invalid(`${name} must be an array of strings.`);
  return value.map((item, index) => text(item, `${name}[${index}]`));
}

async function allPages<T>(read: (cursor?: string) => Promise<Page<T>>): Promise<T[]> {
  const items: T[] = [];
  let cursor: string | undefined;
  for (let pass = 0; pass < MAX_PAGES; pass += 1) {
    const current = await read(cursor);
    items.push(...current.items);
    if (current.next_cursor === null) return items;
    cursor = current.next_cursor;
  }
  return invalid('A paged read exceeded its safety bound.');
}

function entryKey(ref: ThreadRef, seq: string): string {
  return `${ref.owner}\u0000${ref.id}\u0000${seq}`;
}

function mutation(): { idempotency_key: string; retry_until: string } {
  return { idempotency_key: crypto.randomUUID(), retry_until: new Date(Date.now() + 60_000).toISOString() };
}

/** Explicit metadata describes one action; anything else is an intent for the host's SystemOne. */
function mechanicalAction(meta: Record<string, Json>): Action | undefined {
  if (meta.to !== undefined && meta.response !== undefined && meta.execution !== undefined) {
    const to = text(meta.to, 'meta.to');
    const thread = meta.thread === undefined ? undefined : threadRefOf(meta.thread, 'meta.thread');
    const inReplyTo = meta.in_reply_to === undefined ? undefined : messageRefOf(meta.in_reply_to, 'meta.in_reply_to');
    const response = oneOf(meta.response, RESPONSES, 'meta.response');
    const execution = oneOf(meta.execution, EXECUTIONS, 'meta.execution');
    const relation = meta.relation === undefined ? undefined : oneOf(meta.relation, ['reply', 'followup'] as const, 'meta.relation');
    return {
      kind: 'send', to, response, execution,
      ...(thread === undefined ? {} : { thread }),
      ...(inReplyTo === undefined ? {} : { in_reply_to: inReplyTo }),
      ...(relation === undefined ? {} : { relation }),
      ...(meta.context === undefined ? {} : { context: text(meta.context, 'meta.context') }),
      ...(meta.mentions === undefined ? {} : { mentions: stringList(meta.mentions, 'meta.mentions')! }),
    };
  }
  if (meta.scope !== undefined) {
    const scope = oneOf(meta.scope, READ_SCOPES, 'meta.scope');
    if (scope === 'resources') {
      const owners = stringList(meta.owners, 'meta.owners');
      const pageSize = numberField(meta.page_size, 'meta.page_size');
      return {
        kind: 'read', scope, query: {
          op: 'list',
          ...(owners === undefined ? {} : { owners }),
          ...(pageSize === undefined ? {} : { page_size: pageSize }),
          ...(meta.cursor === undefined ? {} : { cursor: text(meta.cursor, 'meta.cursor') }),
        },
      };
    }
    const pageSize = numberField(meta.page_size, 'meta.page_size');
    return {
      kind: 'read', scope,
      ...(meta.thread === undefined ? {} : { thread: threadRefOf(meta.thread, 'meta.thread') }),
      ...(meta.message === undefined ? {} : { message: messageRefOf(meta.message, 'meta.message') }),
      ...(meta.seq === undefined ? {} : { seq: text(meta.seq, 'meta.seq') }),
      ...(meta.cursor === undefined ? {} : { cursor: text(meta.cursor, 'meta.cursor') }),
      ...(pageSize === undefined ? {} : { page_size: pageSize }),
      ...(meta.attention === undefined ? {} : { attention: oneOf(meta.attention, ['inbox', 'thread'] as const, 'meta.attention') }),
    };
  }
  if (meta.thread_op !== undefined) {
    return {
      kind: 'thread', op: oneOf(meta.thread_op, THREAD_OPS, 'meta.thread_op'),
      ...(meta.thread === undefined ? {} : { thread: threadRefOf(meta.thread, 'meta.thread') }),
      ...(meta.to === undefined ? {} : { to: text(meta.to, 'meta.to') }),
      ...(meta.title === undefined ? {} : { title: text(meta.title, 'meta.title') }),
    };
  }
  if (meta.domain_op !== undefined) {
    return {
      kind: 'domain', op: oneOf(meta.domain_op, DOMAIN_OPS, 'meta.domain_op'),
      ...(meta.domain_id === undefined ? {} : { domain_id: text(meta.domain_id, 'meta.domain_id') }),
      ...(meta.expected_revision === undefined ? {} : { expected_revision: text(meta.expected_revision, 'meta.expected_revision') }),
    };
  }
  if (meta.resource !== undefined) {
    return {
      kind: 'resource', resource: resourceRefOf(meta.resource, 'meta.resource'),
      operation: text(meta.operation, 'meta.operation'),
      ...(meta.data === undefined ? {} : { data: meta.data }),
      ...(meta.expected_revision === undefined ? {} : { expected_revision: text(meta.expected_revision, 'meta.expected_revision') }),
    };
  }
  return undefined;
}

function serviceUrl(raw: string): string {
  let url: URL;
  try { url = new URL(raw); }
  catch { return invalid('connect.url must be an absolute HTTP or HTTPS URL.'); }
  if (url.protocol !== 'http:' && url.protocol !== 'https:') invalid('connect.url must be HTTP or HTTPS.');
  if (url.pathname === '/' || url.pathname === '') url.pathname = SERVICE_PATH;
  return url.href.replace(/\/+$/, '');
}

function peerRow(entry: TrustEntry, extra?: Partial<PeerRow>): PeerRow {
  return {
    nodeId: entry.node_id, baseUrl: entry.base_url, fingerprint: entry.fingerprint,
    phase: trustPhase(entry), state: entry.state,
    localApproved: entry.local_approved, remoteApproved: entry.remote_approved,
    verified: false, lastSeen: null, source: null, instanceId: null,
    firstSeen: entry.first_seen, lastChecked: entry.last_checked,
    ...extra,
  };
}

function json(value: unknown): { content: string; contentType: string } {
  return { content: `${JSON.stringify(value, null, 2)}\n`, contentType: 'application/json' };
}

function sendOutcomeOf(value: unknown): SendOutcome | undefined {
  if (value === null || typeof value !== 'object') return undefined;
  const candidate = value as Partial<SendOutcome>;
  return typeof candidate.admission === 'string' ? candidate as SendOutcome : undefined;
}

/**
 * One desktop-bot node: its identity, store and log live in <dataDir>/.omp/onlyne. RPC
 * children are execution contexts of this node; they never host a second node.
 */
export async function createDesktopOnlyne(options: OnlyneBridgeOptions): Promise<OnlyneBridge> {
  const nodeId: NodeId = text(options.nodeId, 'nodeId');
  const peers = options.peers.map((peer, index) => text(peer, `peers[${index}]`));
  const botRoot = resolve(options.dataDir);
  const stateRoot = join(botRoot, '.omp', 'onlyne');
  await mkdir(stateRoot, { recursive: true });

  const logger = createLogger(false, 200, join(stateRoot, 'onlyne.log'));
  const keyDir = await ensureKeyDir(stateRoot, nodeId, 'project');
  const keys: KeyHook = Object.assign(
    () => loadOrCreateKey(keyDir, nodeId),
    { replace: (material: KeyMaterial) => writeKeyMaterial(keyDir, material, { replace: true }) },
  );
  const storage = new FileStorage(join(stateRoot, 'store.json'));
  const contextsNow = (): Array<{ alias: string; summary: string; state: string }> => options.contexts();
  const pending = new Map<string, { exchange: KeyExchange; fingerprint: string; baseUrl: string; at: string }>();
  const done = new Set<string>();
  const doneOrder: string[] = [];
  const inFlight = new Map<string, Promise<void>>();
  let closed = false;
  let server: Bun.Server<undefined> | undefined;
  let host: { close(): Promise<void> } | undefined;
  let node: OnlyneNode | undefined;

  const fingerprintOf = async (): Promise<string> => {
    const identity = await node!.keys.describe();
    if (identity === undefined) throw new OnlyneError('unsupported_delivery', 'The desktop node has no identity key.');
    return identity.fingerprint;
  };

  const cleanup = async (): Promise<void> => {
    closed = true;
    if (node !== undefined) node.client.stopMaintenance();
    try { await host?.close(); } catch (error) { logger.warn('onlyne.host.close.failed', { error: String(error) }); }
    server?.stop(true);
  };

  try {
    let handler: (request: Request) => Promise<Response> = async () =>
      new Response('{"error":{"code":"unavailable","message":"Onlyne is starting.","retryable":true}}', {
        status: 503, headers: { 'content-type': 'application/json' },
      });
    // The library's event stream ticks every 15 s; Bun's default idle timeout is shorter.
    server = Bun.serve({ hostname: BIND, port: 0, idleTimeout: 255, fetch: request => handler(request) });
    const endpoint = `http://${BIND}:${server.port}${SERVICE_PATH}`;
    const instanceId = `${nodeId}:desktop`;

    const authenticate = async (request: Request): Promise<Auth | null> => {
      const actor = request.headers.get('Onlyne-Actor');
      if (actor === null || actor.length === 0) return null;
      if (actor === nodeId) throw new OnlyneError('forbidden', 'A peer cannot claim this desktop node identity.');
      return { actor };
    };
    const resolveTarget = async (id: NodeId) => {
      const target = id === nodeId ? { node_id: id, base_url: endpoint } : await node!.discovery.resolve(id);
      return { ...target, headers: { 'Onlyne-Actor': nodeId } };
    };
    const clock: MaintenanceClock = {
      setTimer: (ms, run) => setTimeout(run, ms),
      clearTimer: handle => clearTimeout(handle as Timer),
    };

    const peerRows = async (): Promise<PeerRow[]> => {
      const [records, entries] = await Promise.all([node!.discovery.listRecords(), node!.trust.list(node!.auth)]);
      const rows = new Map<string, PeerRow>();
      for (const record of records) {
        const entry = entries.find(candidate => candidate.node_id === record.node_id);
        const base = entry === undefined
          ? {
              nodeId: record.node_id, baseUrl: record.base_url, fingerprint: null,
              phase: 'no_exchange' as const, state: null, localApproved: false, remoteApproved: false,
              firstSeen: null, lastChecked: null,
            }
          : peerRow(entry, { baseUrl: record.base_url });
        rows.set(record.node_id, {
          ...base,
          verified: record.verified_at !== undefined && Date.parse(record.expires_at) > Date.now(),
          lastSeen: record.last_seen, source: record.source, instanceId: record.instance_id,
        });
      }
      for (const entry of entries) {
        if (!rows.has(entry.node_id)) rows.set(entry.node_id, peerRow(entry));
      }
      return [...rows.values()].sort((a, b) => a.nodeId.localeCompare(b.nodeId));
    };

    const threadEvidence = async () => ({
      owned: await allPages(cursor => node!.threads.list(node!.auth, { page_size: PAGE, ...(cursor === undefined ? {} : { cursor }) })),
      joined: await allPages(cursor => node!.threads.joined(node!.auth, { page_size: PAGE, ...(cursor === undefined ? {} : { cursor }) })),
    });

    const evidence = async () => {
      const contexts = contextsNow();
      const [neighbors, threads] = await Promise.all([peerRows(), threadEvidence()]);
      return {
        node: { id: nodeId, endpoint, fingerprint: await fingerprintOf() },
        contexts,
        neighbors,
        threads,
        directory: { node: nodeId, endpoint, contexts, peers: await node!.discovery.listRecords() },
      };
    };

    const decide = async (input: MsgInput): Promise<Action> => {
      if (input.meta !== undefined) {
        const mechanical = mechanicalAction(input.meta);
        if (mechanical !== undefined) return mechanical;
      }
      const decision = await options.decide({ ...input, evidence: await evidence() });
      if (decision === null || typeof decision !== 'object' || typeof (decision as { kind?: unknown }).kind !== 'string') {
        throw new OnlyneError('decision_unavailable', 'The SystemOne decision is not an executable action.');
      }
      return decision as Action;
    };

    const emitSafe = (event: DesktopEvent): void => {
      try { options.emit(event); }
      catch (error) { logger.warn('onlyne.emit.failed', { error: String(error) }); }
    };

    const deliver = (entry: Entry, context: string): Promise<'delivered' | 'skipped'> => {
      const key = entryKey(entry.thread_ref, entry.seq);
      if (entry.message.sender === nodeId || done.has(key)) return Promise.resolve('skipped');
      const started = inFlight.get(key);
      if (started !== undefined) return started.then(() => 'skipped');
      const task = options.inbound(entry, context).then(() => {
        done.add(key);
        doneOrder.push(key);
        if (doneOrder.length > MAX_SEEN) {
          const oldest = doneOrder.shift();
          if (oldest !== undefined) done.delete(oldest);
        }
      }).finally(() => { inFlight.delete(key); });
      inFlight.set(key, task);
      return task.then(() => 'delivered');
    };

    const passiveInbound = async (event: Extract<NotifyEvent, { event: 'inbound' }>): Promise<void> => {
      const key = entryKey(event.thread, event.seq);
      if (event.sender === nodeId || done.has(key) || inFlight.has(key)) return;
      const since = event.seq === '1' ? undefined : (BigInt(event.seq) - 1n).toString();
      let entry: Entry | undefined;
      try {
        const listed = await node!.messages.list(node!.auth, event.thread, {
          attention: 'thread', page_size: 10, ...(since === undefined ? {} : { since_seq: since }),
        });
        entry = listed.items.find(item => item.seq === event.seq);
      } catch { /* a foreign owner's thread is answered by the local inbox row below */ }
      const inbox = await node!.messages.inbox(node!.auth, { page_size: PAGE });
      const row = inbox.items.find(item => item.attention.seq === event.seq
        && item.attention.thread_ref.owner === event.thread.owner
        && item.attention.thread_ref.id === event.thread.id);
      entry ??= row?.attention.entry;
      if (entry === undefined) {
        logger.debug('onlyne.inbound.missing', { thread: event.thread.id, seq: event.seq, sender: event.sender });
        return;
      }
      await deliver(entry, row?.context ?? contextsNow()[0]?.alias ?? nodeId);
    };

    const runtime: Runtime = {
      supports: async (_plan: DeliveryPlan) => true,
      async inject(entry: Entry, context?: string, plan?: DeliveryPlan) {
        // The library falls back to the node id when the sender named no context; the bot's
        // own first session is the place its conversations live.
        const named = context ?? plan?.context;
        const landing = named === undefined || named === nodeId ? contextsNow()[0]?.alias ?? named ?? nodeId : named;
        const outcome = await deliver(entry, landing);
        return { context: landing, consumed: true, outcome: outcome === 'delivered' ? 'consumed' as const : 'skipped' as const };
      },
      notify(event: NotifyEvent): void {
        if (event.event === 'trust') {
          emitSafe({
            type: 'onlyne.trust', nodeId: event.entry.node_id, phase: event.phase,
            fingerprint: event.entry.fingerprint, cause: event.cause,
          });
          return;
        }
        if (event.event === 'work-pending') {
          emitSafe({ type: 'onlyne.work-pending', pending: event.pending });
          return;
        }
        emitSafe({ type: 'onlyne.inbound', thread: event.thread, seq: event.seq, sender: event.sender });
        void passiveInbound(event).catch(error => logger.warn('onlyne.inbound.failed', { error: String(error) }));
      },
      trace(event: TraceEvent): void {
        logger.debug(`onlyne.${event.event}`, { ...event });
      },
    };

    const plugins: Plugins = {
      storage, keys, authenticate,
      resolve: resolveTarget,
      decide,
      runtime,
      maintenanceClock: clock,
      domainCandidates: id => node!.discovery.candidates(id),
      executionContexts: async () => contextsNow() as unknown as Json,
      directory: async () => ({ node: nodeId, endpoint, contexts: contextsNow(), peers: await node!.discovery.listRecords() }) as unknown as Json,
      neighbors: async () => peerRows() as unknown as Json,
      // A keyed node authenticated the caller before admission; this hook only permits the
      // library to open a first-contact thread, never to approve a peer.
      inboundThread: async actor => actor !== nodeId,
    };
    node = new OnlyneNode({
      node_id: nodeId, base_url: endpoint, instance_id: instanceId,
      authentication: ['signature'], plugins,
      now: () => Date.now(), id: () => crypto.randomUUID(),
    });

    const localFingerprint = await fingerprintOf();
    installDesktopInboundThread(node, nodeId, contextsNow);
    handler = request => node!.fetch(request);
    node.client.runMaintenance();
    host = await startNodeHost({
      node, endpoint, bind: BIND, seeds: peers, log: logger.log, interactive: false,
    });
    logger.log('onlyne.listening', { node: nodeId, endpoint, fingerprint: localFingerprint, seeds: peers.length });

    const status = async (): Promise<DesktopOnlyneStatus> => {
      const [neighbors, owned, joined, resources, inbox, entries] = await Promise.all([
        peerRows(),
        allPages(cursor => node!.threads.list(node!.auth, { page_size: PAGE, ...(cursor === undefined ? {} : { cursor }) })),
        allPages(cursor => node!.threads.joined(node!.auth, { page_size: PAGE, ...(cursor === undefined ? {} : { cursor }) })),
        node!.resources.list(node!.auth, { page_size: PAGE }),
        node!.messages.inbox(node!.auth, { page_size: PAGE }),
        node!.trust.list(node!.auth),
      ]);
      return {
        node: {
          id: nodeId, endpoint, fingerprint: localFingerprint, instanceId,
          authentication: ['signature'], state: closed ? 'closed' : 'ready',
        },
        trust: {
          localFingerprint,
          peers: entries.map(entry => peerRow(entry, {
            verified: neighbors.find(row => row.nodeId === entry.node_id)?.verified ?? false,
            lastSeen: neighbors.find(row => row.nodeId === entry.node_id)?.lastSeen ?? null,
          })),
          pending: [...pending.values()].map(row => ({
            nodeId: row.exchange.node_id, fingerprint: row.fingerprint, baseUrl: row.baseUrl, at: row.at,
          })),
        },
        neighbors,
        threads: owned,
        joined,
        resources: {
          owners: [{
            owner: nodeId, count: resources.items.length, items: resources.items,
            truncated: resources.next_cursor !== null,
          }],
        },
        contexts: contextsNow(),
        inbox: { pending: inbox.items.filter(item => !item.injected).length },
      };
    };

    const read = async (uri: string): Promise<{ content: string; contentType?: string }> => {
      let url: URL;
      try { url = new URL(uri); }
      catch { return invalid(`read expects an absolute onlyne:// URI, got ${uri}.`); }
      if (url.protocol !== 'onlyne:') invalid(`read expects an onlyne:// URI, got ${uri}.`);
      let parts: string[];
      try { parts = url.pathname.split('/').filter(part => part.length > 0).map(decodeURIComponent); }
      catch { return invalid(`read received an invalid escape in ${uri}.`); }
      const section = url.hostname.toLowerCase();
      const rawPageSize = url.searchParams.get('page_size');
      const pageSize = rawPageSize === null ? undefined : numberField(Number(rawPageSize), 'page_size');
      const cursor = url.searchParams.get('cursor') ?? undefined;

      if (section === 'status') return json(await status());
      if (section === 'neighbors') {
        const rows = await peerRows();
        if (parts.length === 0) return json(rows);
        const row = rows.find(item => item.nodeId === parts[0]);
        if (row === undefined) throw new OnlyneError('not_found', `No peer named ${parts[0]} is observed.`);
        return json(row);
      }
      if (section === 'threads') {
        if (parts.length === 0) {
          return json({
            owned: await allPages(c => node!.threads.list(node!.auth, { page_size: PAGE, cursor: c })),
            joined: await allPages(c => node!.client.joinedThreads({ page_size: PAGE, cursor: c })),
          });
        }
        const ref: ThreadRef = parts.length === 1
          ? { owner: nodeId, id: parts[0]! }
          : { owner: parts[0]!, id: parts[1]! };
        if (parts[2] === 'messages' && parts[3] !== undefined) {
          const seq = parts[3]!;
          const since = seq === '1' ? undefined : (BigInt(seq) - 1n).toString();
          const listed = await node!.client.threadEntries(ref, {
            attention: 'thread', page_size: 10, ...(since === undefined ? {} : { since_seq: since }),
          });
          const entry = listed.items.find(item => item.seq === seq);
          if (entry === undefined) throw new OnlyneError('not_found', `No entry ${seq} in thread ${ref.owner}/${ref.id}.`);
          return json(entry);
        }
        if (parts.length <= 2) {
          const described = await node!.client.describeThread(ref);
          const entries = await node!.client.threadEntries(ref, {
            attention: 'thread', page_size: pageSize ?? PAGE, ...(cursor === undefined ? {} : { cursor }),
          });
          return json({ ...described, entries });
        }
        return invalid(`Unknown onlyne://threads route ${uri}.`);
      }
      if (section === 'messages') {
        if (parts.length !== 2) invalid('onlyne://messages needs <sender>/<id>.');
        return json(await node!.messages.getByRef(node!.auth, { sender: parts[0]!, id: parts[1]! }));
      }
      if (section === 'inbox') {
        const rows: LocalAttention[] = await allPages(c => node!.messages.inbox(node!.auth, {
          page_size: pageSize ?? PAGE, ...(c === undefined ? {} : { cursor: c }),
        }));
        return json(rows);
      }
      if (section === 'resources') {
        if (parts.length === 0) {
          const owners = url.searchParams.getAll('owner');
          return json(await node!.client.readResources({
            op: 'list', ...(owners.length === 0 ? {} : { owners }),
            ...(pageSize === undefined ? {} : { page_size: pageSize }),
            ...(cursor === undefined ? {} : { cursor }),
          }));
        }
        if (parts.length < 2) invalid('onlyne://resources needs <owner>/<id>.');
        const resource: ResourceRef = { owner: parts[0]!, id: parts[1]! };
        const tail = parts[2];
        if (tail === undefined) return json(await node!.client.readResources({ op: 'describe', resource }));
        if (tail === 'log') {
          if (parts[3] !== undefined) return json(await node!.client.readResources({ op: 'event', resource, event_id: parts[3]! }));
          return json(await node!.client.readResources({ op: 'log', resource, ...(pageSize === undefined ? {} : { page_size: pageSize }), ...(cursor === undefined ? {} : { cursor }) }));
        }
        if (tail === 'operations') {
          if (parts[3] !== undefined) return json(await node!.client.readResources({ op: 'status', resource, operation_id: parts[3]! }));
          return json(await node!.client.readResources({ op: 'operations', resource, ...(pageSize === undefined ? {} : { page_size: pageSize }), ...(cursor === undefined ? {} : { cursor }) }));
        }
        return invalid(`Unknown onlyne://resources route ${uri}.`);
      }
      return invalid(`Unknown onlyne:// URI ${uri}.`);
    };

    const action = async (name: string, args: Record<string, unknown>): Promise<unknown> => {
      switch (name) {
        case 'connect': {
          const url = serviceUrl(text(args.url, 'connect.url'));
          const record = await node!.discovery.observe(url, 'desktop-connect');
          const entry = await node!.trust.entry(record.node_id);
          return { action: name, peer: record, trust: entry === undefined ? null : peerRow(entry) };
        }
        case 'trust.request': {
          const id = text(args.nodeId, 'trust.request.nodeId');
          const exchange = await node!.trust.begin(node!.auth, id);
          pending.set(id, {
            exchange, fingerprint: exchange.fingerprint, baseUrl: exchange.base_url,
            at: new Date().toISOString(),
          });
          emitSafe({
            type: 'onlyne.trust.request', nodeId: exchange.node_id,
            fingerprint: exchange.fingerprint, localFingerprint,
          });
          return {
            action: name, nodeId: exchange.node_id, fingerprint: exchange.fingerprint,
            baseUrl: exchange.base_url, localFingerprint, phase: 'awaiting-approval',
          };
        }
        case 'trust.approve': {
          const id = text(args.nodeId, 'trust.approve.nodeId');
          const fingerprint = text(args.fingerprint, 'trust.approve.fingerprint');
          const held = pending.get(id);
          if (held === undefined) throw new OnlyneError('invalid_request', `No displayed fingerprint for ${id}; call trust.request first.`);
          if (fingerprint !== held.fingerprint) {
            throw new OnlyneError('unauthenticated', 'The approved fingerprint is not the one displayed.', { fingerprint: held.fingerprint });
          }
          const entry = await held.exchange.confirm(fingerprint);
          pending.delete(id);
          return { action: name, nodeId: id, entry: peerRow(entry), phase: trustPhase(entry) };
        }
        case 'trust.revoke': {
          const id = text(args.nodeId, 'trust.revoke.nodeId');
          await node!.trust.revoke(node!.auth, id);
          pending.delete(id);
          return { action: name, nodeId: id };
        }
        case 'thread.read': {
          const ref: ThreadRef = { owner: text(args.owner, 'thread.read.owner'), id: text(args.id, 'thread.read.id') };
          const described = await node!.client.describeThread(ref);
          const pageSize = numberField(args.page_size, 'thread.read.page_size');
          const entries = await node!.client.threadEntries(ref, {
            attention: 'thread',
            ...(pageSize === undefined ? {} : { page_size: pageSize }),
            ...(args.cursor === undefined ? {} : { cursor: text(args.cursor, 'thread.read.cursor') }),
          });
          return { action: name, threadRef: ref, thread: described.thread, members: described.members, entries };
        }
        case 'resource.list': {
          const owners = stringList(args.owners, 'resource.list.owners');
          const pageSize = numberField(args.page_size, 'resource.list.page_size');
          const inventory = await node!.client.readResources({
            op: 'list',
            ...(owners === undefined ? {} : { owners }),
            ...(pageSize === undefined ? {} : { page_size: pageSize }),
            ...(args.cursor === undefined ? {} : { cursor: text(args.cursor, 'resource.list.cursor') }),
          });
          return { action: name, inventory };
        }
        default:
          return invalid(`Unknown Onlyne action ${name}.`);
      }
    };

    return {
      status,
      async dispatch(input, context: HostToolContext) {
        const typed: MsgInput = {
          delivery_kind: text(input.delivery_kind, 'delivery_kind'),
          msg: input.msg,
          ...(input.meta === undefined ? {} : { meta: jsonRecord(input.meta, 'meta') }),
        };
        const outcome = await node!.client.msg(typed, { signal: context.signal, context: context.conversation.id });
        const send = sendOutcomeOf(outcome);
        // An awaited body is handed to this conversation as a tool result; confirming it here is
        // what keeps the automatic fragment from presenting the same reply a second time.
        if (send?.reply !== undefined && send.reply_body !== 'withheld') {
          await node!.client.confirmToolResult(send.reply, context.conversation.id);
        }
        return outcome;
      },
      read,
      action,
      async close() {
        if (closed) return;
        await cleanup();
        logger.log('onlyne.stopped', { node: nodeId });
      },
    };
  } catch (error) {
    await cleanup();
    throw error;
  }
}

/**
 * A first contact names a stable, desktop-owned thread instead of the library's random
 * first-contact slot. Replies and explicit threads are left untouched: the library's permit
 * and owner rules decide where they land, and a foreign thread must not be invented here.
 */
function installDesktopInboundThread(
  node: OnlyneNode, owner: NodeId,
  contexts: () => Array<{ alias: string; summary: string; state: string }>,
): void {
  const admit = node.messages.admit.bind(node.messages);
  node.messages.admit = async (auth: Auth, request: SendRequest) => {
    const message = request.message;
    if (message.meta?.thread !== undefined || message.meta?.in_reply_to !== undefined) {
      return admit(auth, request);
    }
    const alias = request.plan.context;
    const known = alias !== undefined && contexts().some(context => context.alias === alias);
    const self = message.sender === owner;
    const scope = known ? `context:${encodeURIComponent(alias)}` : self ? 'self' : `peer:${encodeURIComponent(message.sender)}`;
    const id = `desktop:${scope}`;
    const ref: ThreadRef = { owner, id };
    const title = known ? `desktop:${alias}` : `desktop:${self ? owner : message.sender}`;
    await ensureDesktopThread(node, ref, title);
    if (!self) await ensureDesktopMember(node, ref, message.sender);
    const receipt = await admit(auth, {
      ...request,
      message: { ...message, meta: { ...message.meta, thread: ref } },
    });
    // The thread exists before admission, so the library cannot report it as created; on a
    // stable desktop thread, sequence 1 is still the conversation's first message.
    return receipt.thread_created === true || receipt.seq !== '1' ? receipt : { ...receipt, thread_created: true };
  };
}

async function ensureDesktopThread(node: OnlyneNode, ref: ThreadRef, title: string): Promise<void> {
  try {
    await node.threads.describe(node.auth, ref);
    return;
  } catch (error) {
    if (!(error instanceof OnlyneError) || error.error.code !== 'not_found') throw error;
  }
  try {
    await node.threads.create(node.auth, { ...mutation(), title, id: ref.id });
  } catch (error) {
    if (!(error instanceof OnlyneError) || error.error.code !== 'id_conflict') throw error;
  }
}

async function ensureDesktopMember(node: OnlyneNode, ref: ThreadRef, peer: NodeId): Promise<void> {
  const described = await node.threads.describe(node.auth, ref);
  const existing = described.members.find(member => member.peer === peer);
  if (existing?.state === 'active' && existing.access.includes('post')) return;
  await node.threads.member(node.auth, ref, peer, { state: 'active', access: ['post', 'read'] }, mutation());
}
