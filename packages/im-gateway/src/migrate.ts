import { createHash } from 'node:crypto';
import { readFile, rename, stat, unlink, writeFile } from 'node:fs/promises';
import { OnlyneError, type NodeId, type ResourceRef, type ThreadRef } from '@onlyne/lib';
import { gatewayNodeId } from './config.js';
import { notifyMessageId } from './pusher.js';
import { t } from './gateway-text.js';

/**
 * Offline owner migration (RFC 0012 §2): one channel per node means the endpoint identity changes,
 * while the conversations, bindings, cursors and logs must survive. The library is unchanged, so
 * this module rewrites the store the library owns: every key part and every authenticated fact
 * that names the old endpoint becomes the new one, and nothing else in a message body is touched.
 *
 * Nothing here runs against a live store: the caller passes a path, the store is read twice, a
 * byte-identical snapshot is written first, and the replacement is one atomic rename. Rollback
 * restores that snapshot and keeps the migrated store as a safety copy.
 */

type Rec = Record<string, unknown>;
type Store = Record<string, unknown>;

const isRec = (value: unknown): value is Rec => value !== null && typeof value === 'object' && !Array.isArray(value);

function requireRec(value: unknown, what: string): Rec {
  if (!isRec(value)) throw new OnlyneError('invalid_request', `${what} is not an object.`);
  return value;
}

/** The library's canonical form (core.ts:40), so document digests recompute to the same bytes. */
function canonical(value: unknown): string {
  if (value === undefined) return 'null';
  if (value === null || typeof value !== 'object') return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonical).join(',')}]`;
  const record = value as Rec;
  return `{${Object.keys(record)
    .filter(key => record[key] !== undefined)
    .sort()
    .map(key => `${JSON.stringify(key)}:${canonical(record[key])}`)
    .join(',')}}`;
}

/** The library's message digest (messages.ts:37): `SHA-256` over `{message, target_endpoint}`. */
async function messageDigest(request: { message: unknown; target_endpoint: unknown }): Promise<string> {
  return createHash('sha256')
    .update(new TextEncoder().encode(canonical({ message: request.message, target_endpoint: request.target_endpoint })))
    .digest('hex');
}

/** The library's operation digest (resources.ts:140): plain canonical JSON, not a hash. */
function operationDigest(actor: string, ref: ResourceRef, request: Rec): string {
  return JSON.stringify({
    actor, resource_ref: ref, operation: request.operation,
    expected_revision: request.expected_revision, data: request.data,
  });
}

const stamp = (ms: number): string => new Date(ms).toISOString().replace(/[:.]/g, '-');
const safeName = (value: string): string => value.replace(/[^A-Za-z0-9._-]/g, '_');

export interface MigrationPairing {
  node_id: NodeId;
  /** The new node key's fingerprint, when the operator already knows it; recorded, never verified here. */
  fingerprint?: string;
}

export interface MigrationInput {
  store_path: string;
  from: NodeId;
  to: NodeId;
  /** The channel platform the new node serves; the new identity must belong to it. */
  platform: string;
  /** Explicit pairing of the new identity. Trust is never inherited from the old endpoint. */
  pairing: MigrationPairing;
  snapshot_path?: string;
  report_path?: string;
  now?: () => number;
}

export interface ResourceMove {
  from: ResourceRef;
  to: ResourceRef;
  thread: { from: ThreadRef; to: ThreadRef };
}

export interface MigrationReport {
  store_path: string;
  from: NodeId;
  to: NodeId;
  platform: string;
  snapshot_path: string;
  report_path: string;
  resources: ResourceMove[];
  threads: number;
  entries: number;
  events: number;
  operations: number;
  pushes: number;
  rendering_in_flight: number;
  /** Messages the old endpoint authored (split projections) re-attributed to the new identity. */
  reattributed_messages: number;
  keys_rewritten: number;
  values_rewritten: number;
  digests_recomputed: number;
  fenced_idempotency: string[];
  probes_archived: number;
  trust_retained: NodeId[];
  warnings: string[];
}

export interface RollbackResult {
  store_path: string;
  snapshot_path: string;
  safety_path: string;
}

interface Stats {
  resources: ResourceMove[];
  threads: number;
  entries: number;
  events: number;
  operations: number;
  pushes: number;
  rendering_in_flight: number;
  reattributed_messages: number;
  keys_rewritten: number;
  values_rewritten: number;
  digests_recomputed: number;
  fenced_idempotency: string[];
  probes_archived: number;
  trust_retained: NodeId[];
  warnings: string[];
}

function validate(input: MigrationInput): void {
  if (input.from.length === 0 || input.to.length === 0) {
    throw new OnlyneError('invalid_request', t('migrate.useApply'));
  }
  if (input.from === input.to) throw new OnlyneError('invalid_request', t('migrate.fromEqualsTo', { node: input.from }));
  const expected = gatewayNodeId(input.platform);
  if (input.to !== expected && !input.to.startsWith(`${expected}-`)) {
    throw new OnlyneError('invalid_request', t('migrate.identity', { node: input.to, platform: input.platform, expected }));
  }
  if (input.pairing === undefined || input.pairing.node_id !== input.to) {
    throw new OnlyneError('invalid_request', t('migrate.pair', { expected: input.to }));
  }
}

function parseStore(text: string, path: string): Store {
  let parsed: unknown;
  try { parsed = JSON.parse(text); } catch { throw new OnlyneError('invalid_request', t('migrate.notObject', { path })); }
  if (!isRec(parsed)) throw new OnlyneError('invalid_request', t('migrate.notObject', { path }));
  return parsed;
}

/**
 * Reads the store and answers what a migration would write, without writing anything. Pending
 * handovers and unsettled operations are refused here: an offline rewrite must not invent their
 * settlement, and their probes are bound to the old resource reference.
 */
export async function planMigration(input: MigrationInput): Promise<{ store: Store; report: MigrationReport }> {
  validate(input);
  const text = await readFile(input.store_path, 'utf8');
  const store = parseStore(text, input.store_path);
  const { store: migrated, stats } = await rewrite(store, input);
  const now = (input.now ?? Date.now)();
  const snapshot_path = input.snapshot_path ?? `${input.store_path}.snapshot-${stamp(now)}`;
  const report_path = input.report_path ?? `${input.store_path}.migration-${safeName(input.from)}-to-${safeName(input.to)}.json`;
  return {
    store: migrated,
    report: {
      store_path: input.store_path,
      from: input.from,
      to: input.to,
      platform: input.platform,
      snapshot_path,
      report_path,
      ...stats,
    },
  };
}

/**
 * Migrates one offline store. The snapshot is written and verified before the store is touched,
 * the store's size and mtime are compared immediately before the rename, and the report lists
 * every moved resource and thread plus what could not be rewritten.
 */
export async function applyMigration(input: MigrationInput): Promise<MigrationReport> {
  validate(input);
  const original = await readFile(input.store_path, 'utf8');
  const store = parseStore(original, input.store_path);
  const { store: migrated, stats } = await rewrite(store, input);
  const now = (input.now ?? Date.now)();
  const snapshot = input.snapshot_path ?? `${input.store_path}.snapshot-${stamp(now)}`;
  const reportPath = input.report_path ?? `${input.store_path}.migration-${safeName(input.from)}-to-${safeName(input.to)}.json`;

  await writeFile(snapshot, original, { flag: 'wx' });
  if (await readFile(snapshot, 'utf8') !== original) {
    throw new OnlyneError('invalid_request', t('migrate.snapshot', { path: snapshot }));
  }

  const before = await stat(input.store_path);
  const temp = `${input.store_path}.migrating-${process.pid}-${now}`;
  await writeFile(temp, JSON.stringify(migrated));
  const after = await stat(input.store_path);
  if (before.mtimeMs !== after.mtimeMs || before.size !== after.size) {
    await unlink(temp).catch(() => undefined);
    throw new OnlyneError('invalid_request', t('migrate.storeMoved'));
  }
  await rename(temp, input.store_path);

  const report: MigrationReport = {
    store_path: input.store_path,
    from: input.from,
    to: input.to,
    platform: input.platform,
    snapshot_path: snapshot,
    report_path: reportPath,
    ...stats,
  };
  await writeFile(reportPath, `${JSON.stringify(report, null, 2)}\n`);
  return report;
}

/**
 * Restores a migration snapshot byte for byte. The current store is saved first under
 * `.rollback-<time>`, so a wrong rollback is itself recoverable.
 */
export async function rollbackMigration(input: { store_path: string; snapshot_path: string; now?: () => number }): Promise<RollbackResult> {
  let snapshot: string;
  try {
    snapshot = await readFile(input.snapshot_path, 'utf8');
  } catch {
    throw new OnlyneError('invalid_request', t('migrate.rollbackMissing', { path: input.snapshot_path }));
  }
  parseStore(snapshot, input.snapshot_path);
  const now = (input.now ?? Date.now)();
  const safety = `${input.store_path}.rollback-${stamp(now)}`;
  const current = await readFile(input.store_path, 'utf8').catch(() => undefined);
  if (current !== undefined) await writeFile(safety, current, { flag: 'wx' });
  await writeFile(input.store_path, snapshot);
  return { store_path: input.store_path, snapshot_path: input.snapshot_path, safety_path: safety };
}

/** A read-only look at which identities and counts a store holds. */
export async function inspectStore(store_path: string): Promise<{
  keys: number;
  resources: ResourceRef[];
  threads: ThreadRef[];
}> {
  const store = parseStore(await readFile(store_path, 'utf8'), store_path);
  const resources: ResourceRef[] = [];
  const threads: ThreadRef[] = [];
  for (const key of Object.keys(store)) {
    const parts = key.split('/').map(decodePart);
    if (parts[0] === 'resource' && parts.length === 3 && parts[1] !== undefined && parts[2] !== undefined) {
      resources.push({ owner: parts[1], id: parts[2] });
    }
    if (parts[0] === 'thread' && parts.length === 3 && parts[1] !== undefined && parts[2] !== undefined) {
      threads.push({ owner: parts[1], id: parts[2] });
    }
  }
  return { keys: Object.keys(store).length, resources, threads };
}

function decodePart(part: string): string {
  try { return decodeURIComponent(part); } catch { return part; }
}

async function rewrite(store: Store, input: MigrationInput): Promise<{ store: Store; stats: Stats }> {
  const { from, to } = input;
  const mapId = (value: unknown): unknown => (value === from ? to : value);
  const stats: Stats = {
    resources: [], threads: 0, entries: 0, events: 0, operations: 0, pushes: 0,
    rendering_in_flight: 0, reattributed_messages: 0, keys_rewritten: 0, values_rewritten: 0, digests_recomputed: 0,
    fenced_idempotency: [], probes_archived: 0, trust_retained: [], warnings: [],
  };

  // Refuse a store with work the rewrite cannot settle. The operator can let the maintenance
  // ring finish and rerun; rewriting an open handover would leave its probe bound to the old ref.
  let owned = 0;
  for (const [key, value] of Object.entries(store)) {
    const parts = key.split('/').map(decodePart);
    if (parts[0] === 'resource' && parts[1] === from && parts.length === 3) owned += 1;
    if (parts[0] === 'resource' && parts[1] === from && parts[3] === 'handover' && isRec(value) && value.state === 'open') {
      throw new OnlyneError('invalid_request', t('migrate.pendingHandover', { resource: `${from}/${parts[2] ?? ''}` }));
    }
    if (parts[0] === 'resource' && parts[1] === from && parts[3] === 'operation' && isRec(value) && value.settle === true) {
      const result = isRec(value.record) && isRec(value.record.result) ? value.record.result : undefined;
      const state = result?.state;
      if (state !== 'committed' && state !== 'aborted') {
        throw new OnlyneError('invalid_request', t('migrate.pendingOperation', { resource: `${from}/${parts[2] ?? ''}` }));
      }
    }
  }
  if (owned === 0) throw new OnlyneError('invalid_request', t('migrate.noResources', { node: from }));

  const remapRef = (value: unknown): unknown =>
    isRec(value) && typeof value.owner === 'string' ? { ...value, owner: mapId(value.owner) } : value;
  const remapThread = (value: unknown): unknown =>
    isRec(value) && typeof value.owner === 'string' ? { ...value, owner: mapId(value.owner) } : value;
  const remapMessageRef = (value: unknown): unknown =>
    isRec(value) && typeof value.sender === 'string' ? { ...value, sender: mapId(value.sender) } : value;
  const remapData = (value: unknown): unknown => {
    if (!isRec(value)) return value;
    return {
      ...value,
      ...(isRec(value.resource_ref) ? { resource_ref: remapRef(value.resource_ref) } : {}),
      ...(isRec(value.thread) ? { thread: remapThread(value.thread) } : {}),
      ...(isRec(value.in_reply_to) ? { in_reply_to: remapMessageRef(value.in_reply_to) } : {}),
    };
  };
  const remapMeta = (value: unknown): unknown => {
    if (!isRec(value)) return value;
    return {
      ...value,
      ...(isRec(value.thread) ? { thread: remapThread(value.thread) } : {}),
      ...(isRec(value.in_reply_to) ? { in_reply_to: remapMessageRef(value.in_reply_to) } : {}),
      ...(isRec(value.data) ? { data: remapData(value.data) } : {}),
    };
  };
  const remapMessage = (value: unknown): unknown => {
    if (!isRec(value)) return value;
    return { ...value, sender: mapId(value.sender), meta: remapMeta(value.meta) };
  };
  const remapEntry = (value: unknown): unknown => {
    if (!isRec(value)) return value;
    return {
      ...value,
      thread_ref: remapThread(value.thread_ref),
      message: remapMessage(value.message),
      authenticated_author: mapId(value.authenticated_author),
      reply_authority: mapId(value.reply_authority),
    };
  };
  const remapReadScope = (value: unknown): unknown => {
    if (!isRec(value)) return value;
    return {
      ...value,
      ...(Array.isArray(value.peers) ? { peers: value.peers.map(mapId) } : {}),
    };
  };
  const remapMember = (value: unknown): unknown => {
    if (!isRec(value)) return value;
    return { ...value, peer: mapId(value.peer), read_scope: remapReadScope(value.read_scope) };
  };
  const remapGrant = (value: unknown): unknown => {
    if (!isRec(value)) return value;
    return { ...value, recipient: mapId(value.recipient), thread_ref: remapThread(value.thread_ref) };
  };
  const remapSettlement = (value: unknown): unknown => {
    if (!isRec(value)) return value;
    return {
      ...value,
      ...(isRec(value.resource_ref) ? { resource_ref: remapRef(value.resource_ref) } : {}),
      ...('handler' in value ? { handler: mapId(value.handler) } : {}),
    };
  };
  /**
   * An obligation id is `key(owner, thread, sender, message, recipient)`: five encoded parts in one
   * string. A shape that is not five parts is left alone and reported, never guessed at.
   */
  const remapObligation = (value: unknown): unknown => {
    if (typeof value !== 'string') return value;
    const parts = value.split('/').map(decodePart);
    if (parts.length !== 5) {
      stats.warnings.push(`An obligation id has an unexpected shape and was left alone: ${value}`);
      return value;
    }
    parts[0] = String(mapId(parts[0] ?? ''));
    parts[2] = String(mapId(parts[2] ?? ''));
    parts[4] = String(mapId(parts[4] ?? ''));
    return parts.map(encodeURIComponent).join('/');
  };
  const remapAttention = (value: unknown): unknown => {
    if (!isRec(value)) return value;
    return {
      ...value,
      recipient: mapId(value.recipient),
      thread_ref: remapThread(value.thread_ref),
      message_ref: remapMessageRef(value.message_ref),
      authenticated_author: mapId(value.authenticated_author),
      obligation_id: remapObligation(value.obligation_id),
      ...(isRec(value.entry) ? { entry: remapEntry(value.entry) } : {}),
    };
  };
  const remapReceipt = (value: unknown): unknown => {
    if (!isRec(value)) return value;
    return {
      ...value,
      message_ref: remapMessageRef(value.message_ref),
      thread_ref: remapThread(value.thread_ref),
      ...(isRec(value.in_reply_to) ? { in_reply_to: remapMessageRef(value.in_reply_to) } : {}),
    };
  };
  const remapOutbound = async (value: unknown): Promise<unknown> => {
    if (!isRec(value)) return value;
    const request = isRec(value.request)
      ? { ...value.request, target_endpoint: mapId(value.request.target_endpoint), message: remapMessage(value.request.message) }
      : value.request;
    let digest = value.digest;
    if (typeof digest === 'string' && isRec(request) && isRec(request.message)) {
      digest = await messageDigest({ message: request.message, target_endpoint: request.target_endpoint });
      stats.digests_recomputed += 1;
    }
    return {
      ...value,
      request,
      expected_author: mapId(value.expected_author),
      ...(isRec(value.receipt) ? { receipt: remapReceipt(value.receipt) } : {}),
      ...(typeof digest === 'string' ? { digest } : {}),
    };
  };

  const next = new Map<string, unknown>();
  const put = (key: string, value: unknown, originalKey: string): void => {
    if (key !== originalKey && (next.has(key) || Object.prototype.hasOwnProperty.call(store, key))) {
      throw new OnlyneError('invalid_request', `The migration would overwrite an existing store row at ${key}.`);
    }
    if (key !== originalKey) stats.keys_rewritten += 1;
    next.set(key, value);
  };

  for (const [key, value] of Object.entries(store)) {
    const parts = key.split('/').map(decodePart);
    const head = parts[0];
    const encoded = (): string => parts.map(encodeURIComponent).join('/');
    let changed = false;
    const remapOwnerAt = (index: number): void => {
      if (parts[index] === from) { parts[index] = to; changed = true; }
    };

    switch (head) {
      case 'resource': {
        if (parts[1] !== from) break;
        remapOwnerAt(1);
        const kind = parts[3];
        if (kind === 'event') {
          stats.events += 1;
          const nextValue = isRec(value) ? { ...value, resource_ref: remapRef(value.resource_ref) } : value;
          put(encoded(), nextValue, key);
          continue;
        }
        if (kind === 'source') {
          put(encoded(), value, key);
          continue;
        }
        if (kind === 'operation') {
          const intent = requireRec(value, `The operation intent at ${key}`);
          const rec = requireRec(intent.record, `The operation record at ${key}`);
          const actor = mapId(rec.actor);
          const ref = remapRef(rec.resource_ref);
          const typedRef: ResourceRef | undefined = isRec(ref) && typeof ref.owner === 'string' && typeof ref.id === 'string'
            ? { owner: ref.owner, id: ref.id }
            : undefined;
          const request = isRec(rec.request) ? rec.request : undefined;
          const digest = request !== undefined && typeof actor === 'string' && typedRef !== undefined
            ? operationDigest(actor, typedRef, request)
            : rec.digest;
          if (digest !== rec.digest) stats.digests_recomputed += 1;
          stats.operations += 1;
          put(encoded(), { ...intent, record: { ...rec, actor, resource_ref: ref, digest } }, key);
          continue;
        }
        if (kind === 'handover') {
          const intent = requireRec(value, `The handover intent at ${key}`);
          const ref = remapRef(intent.resource_ref);
          const typedRef: ResourceRef | undefined = isRec(ref) && typeof ref.owner === 'string' && typeof ref.id === 'string'
            ? { owner: ref.owner, id: ref.id }
            : undefined;
          const initiator = mapId(intent.initiator);
          let digest = intent.digest;
          const operationKey = ['resource', from, parts[2] ?? '', 'operation', String(intent.operation_id)].map(encodeURIComponent).join('/');
          const operation = store[operationKey];
          const rec = isRec(operation) && isRec(operation.record) ? operation.record : undefined;
          const request = rec !== undefined && isRec(rec.request) ? rec.request : undefined;
          const actor = rec !== undefined ? mapId(rec.actor) : undefined;
          if (request !== undefined && typeof actor === 'string' && typedRef !== undefined) {
            digest = operationDigest(actor, typedRef, request);
            if (digest !== intent.digest) stats.digests_recomputed += 1;
          }
          put(encoded(), { ...intent, resource_ref: ref, initiator, digest }, key);
          continue;
        }
        const resource = requireRec(value, `The resource at ${key}`);
        const ref = remapRef(resource.resource_ref);
        const binding = isRec(resource.binding) ? { ...resource.binding, handler: mapId(resource.binding.handler) } : resource.binding;
        const logReaders = Array.isArray(resource.log_readers) ? resource.log_readers.map(mapId) : resource.log_readers;
        const moved = { ...resource, resource_ref: ref, binding, log_readers: logReaders };
        if (isRec(ref) && typeof ref.id === 'string') {
          stats.resources.push({
            from: { owner: from, id: ref.id },
            to: { owner: to, id: ref.id },
            thread: { from: { owner: from, id: `im-${ref.id}` }, to: { owner: to, id: `im-${ref.id}` } },
          });
        }
        put(encoded(), moved, key);
        continue;
      }
      case 'thread': {
        if (parts[1] !== from) break;
        remapOwnerAt(1);
        stats.threads += 1;
        const thread = isRec(value) ? { ...value, thread_ref: remapThread(value.thread_ref) } : value;
        put(encoded(), thread, key);
        continue;
      }
      case 'entry': {
        if (parts[1] !== from) break;
        remapOwnerAt(1);
        stats.entries += 1;
        put(encoded(), remapEntry(value), key);
        continue;
      }
      case 'event': {
        if (parts[1] !== from) break;
        remapOwnerAt(1);
        if (isRec(value)) {
          const event: Rec = {
            ...value,
            ...(isRec(value.thread_ref) ? { thread_ref: remapThread(value.thread_ref) } : {}),
            ...(isRec(value.resource_ref) ? { resource_ref: remapRef(value.resource_ref) } : {}),
            ...(isRec(value.message_ref) ? { message_ref: remapMessageRef(value.message_ref) } : {}),
          };
          if (value.type === 'membership') event.data = remapMember(value.data);
          put(encoded(), event, key);
        } else {
          put(encoded(), value, key);
        }
        continue;
      }
      case 'member': {
        if (parts[1] !== from) break;
        remapOwnerAt(1);
        put(encoded(), remapMember(value), key);
        continue;
      }
      case 'grant': {
        if (parts[1] !== from) break;
        remapOwnerAt(1);
        put(encoded(), remapGrant(value), key);
        continue;
      }
      case 'known': {
        if (parts[1] !== from) break;
        remapOwnerAt(1);
        const known = isRec(value) ? { ...value, owner: mapId(value.owner), thread_ref: remapThread(value.thread_ref) } : value;
        put(encoded(), known, key);
        continue;
      }
      case 'attention': {
        if (parts[1] !== from) break;
        remapOwnerAt(1);
        if (parts[3] !== undefined) parts[3] = String(remapObligation(parts[3]));
        put(encoded(), remapAttention(value), key);
        continue;
      }
      case 'inbox': {
        const obligation = parts[1] ?? '';
        const sub = obligation.split('/').map(decodePart);
        if (sub.length !== 5) {
          stats.warnings.push(`An inbox row has an unexpected obligation shape and was left alone: ${key}`);
          put(key, value, key);
          continue;
        }
        sub[0] = String(mapId(sub[0] ?? ''));
        sub[2] = String(mapId(sub[2] ?? ''));
        sub[4] = String(mapId(sub[4] ?? ''));
        parts[1] = sub.map(encodeURIComponent).join('/');
        const local = isRec(value) ? { ...value, attention: remapAttention(value.attention) } : value;
        if (isRec(value) && isRec(value.settlement)) (local as Rec).settlement = remapSettlement(value.settlement);
        put(encoded(), local, key);
        continue;
      }
      case 'default': {
        if (parts[1] !== from) break;
        remapOwnerAt(1);
        put(encoded(), isRec(value) ? { ...value, owner: mapId(value.owner) } : value, key);
        continue;
      }
      case 'message': {
        // The row is the message identity tombstone (`{digest, receipt, retry_until}`): its author
        // is the key part, and its receipt names the thread and the message it answers.
        if (parts[1] === from) {
          parts[1] = to;
          changed = true;
          stats.reattributed_messages += 1;
        }
        const remembered = isRec(value)
          ? { ...value, ...(isRec(value.receipt) ? { receipt: remapReceipt(value.receipt) } : {}) }
          : value;
        if (changed) put(encoded(), remembered, key);
        else next.set(key, remembered);
        continue;
      }
      case 'entry-ref': {
        if (parts[1] === from) { parts[1] = to; changed = true; }
        const entry = remapEntry(value);
        if (changed) put(encoded(), entry, key);
        else next.set(key, entry);
        continue;
      }
      case 'message-key': {
        const actor = parts[1];
        if (actor !== from) {
          next.set(key, value);
          continue;
        }
        // The row holds a digest over `{message, target_endpoint}` but not the request, so it
        // cannot be recomputed. Fencing it under `archive/` keeps the evidence and lets the
        // identity tombstone (`message/<sender>/<id>`) answer a retry instead of the bound digest.
        const identity = isRec(value) && typeof value.identity === 'string' ? value.identity : undefined;
        const remappedIdentity = identity === undefined ? undefined : (() => {
          const identityParts = identity.split('/').map(decodePart);
          if (identityParts.length < 3) return identity;
          if (identityParts[1] === from) identityParts[1] = to;
          return identityParts.map(encodeURIComponent).join('/');
        })();
        if (remappedIdentity !== undefined && store[remappedIdentity] === undefined) {
          stats.warnings.push(`An idempotency row for ${key} was fenced although its message identity is missing; a retry of that send may repeat it.`);
        }
        const fenced = `archive/${key}`;
        put(fenced, value, key);
        stats.fenced_idempotency.push(key);
        continue;
      }
      case 'outbound': {
        if (parts[1] === from) { parts[1] = to; changed = true; }
        const outbound = await remapOutbound(value);
        if (changed) put(encoded(), outbound, key);
        else next.set(key, outbound);
        continue;
      }
      case 'reply': {
        // `reply` is an in-process waiter key, not a durable row (client.ts:139). Anything found
        // here is left as it is and reported rather than guessed at.
        if (JSON.stringify(value).includes(from)) {
          stats.warnings.push(`A reply row at ${key} names ${from}; it was left alone.`);
        }
        next.set(key, value);
        continue;
      }
      case 'permit': {
        if (parts[1] === from) { parts[1] = to; changed = true; }
        if (isRec(value)) {
          const permit = {
            ...value,
            message_ref: remapMessageRef(value.message_ref),
            grantee: mapId(value.grantee),
            landing: remapThread(value.landing),
          };
          if (changed) put(encoded(), permit, key);
          else next.set(key, permit);
          continue;
        }
        if (changed) put(encoded(), value, key);
        else next.set(key, value);
        continue;
      }
      case 'consumed': {
        if (parts[2] !== from) {
          next.set(key, value);
          continue;
        }
        parts[2] = to;
        const consumed = isRec(value) ? { ...value, thread_ref: remapThread(value.thread_ref) } : value;
        put(encoded(), consumed, key);
        continue;
      }
      case 'todo': {
        if (parts[2] !== from) {
          next.set(key, value);
          continue;
        }
        parts[2] = to;
        const todo = isRec(value) ? { ...value, entry: remapEntry(value.entry) } : value;
        put(encoded(), todo, key);
        continue;
      }
      case 'notice': {
        put(key, isRec(value) ? { ...value, thread_ref: remapThread(value.thread_ref), recipient: mapId(value.recipient) } : value, key);
        continue;
      }
      case 'probe': {
        if (JSON.stringify(value).includes(from)) stats.probes_archived += 1;
        next.set(key, value);
        continue;
      }
      case 'trust': {
        const peer = parts[1];
        if (peer !== undefined) stats.trust_retained.push(peer);
        next.set(key, value);
        continue;
      }
      case 'trust-outbound':
      case 'security':
      case 'nonce':
      case 'domain':
      case 'decision':
      case 'idempotency': {
        if (JSON.stringify(value).includes(from)) {
          stats.warnings.push(`A ${head} row still names ${from}; it was not rewritten because its semantics are outside the owner remap.`);
        }
        next.set(key, value);
        continue;
      }
      case 'archive': {
        next.set(key, value);
        continue;
      }
      case 'im': {
        const scope = parts[1];
        if (scope === 'conversation') {
          const row = isRec(value) ? { ...value, resource_ref: remapRef(value.resource_ref) } : value;
          next.set(key, row);
          continue;
        }
        if (parts[2] === from) { parts[2] = to; changed = true; }
        if (scope === 'handle' && isRec(value)) {
          put(encoded(), { ...value, resource_ref: remapRef(value.resource_ref) }, key);
          continue;
        }
        if (scope === 'push') {
          const push = isRec(value) ? value : undefined;
          const refValue = push === undefined ? undefined : push.ref;
          if (push !== undefined && isRec(refValue) && typeof refValue.owner === 'string' && typeof refValue.id === 'string') {
            const originalRef: ResourceRef = { owner: refValue.owner, id: refValue.id };
            const event = isRec(push.event) ? push.event : undefined;
            const carried = {
              ...push,
              // A retry after the move must reuse the projection id the old endpoint would have
              // sent, so a push a peer already accepted is not duplicated under a new identity.
              message_id: typeof push.message_id === 'string'
                ? push.message_id
                : (typeof event?.event_id === 'string' ? notifyMessageId(originalRef, event.event_id) : undefined),
              ref: remapRef(refValue),
              event: event === undefined ? push.event : { ...event, resource_ref: remapRef(event.resource_ref) },
            };
            stats.pushes += 1;
            put(encoded(), carried, key);
            continue;
          }
          if (changed) put(encoded(), value, key);
          else next.set(key, value);
          continue;
        }
        if (scope === 'render' && isRec(value) && value.state === 'rendering') stats.rendering_in_flight += 1;
        if (changed) put(encoded(), value, key);
        else next.set(key, value);
        continue;
      }
      default: {
        if (parts[0] === from) {
          throw new OnlyneError('invalid_request', `The store row ${key} starts with the old identity and this migration has no rule for it.`);
        }
        if (JSON.stringify(value).includes(from)) {
          stats.warnings.push(`The store row ${key} still names ${from}; it was not rewritten because its namespace is outside the owner remap.`);
        }
        next.set(key, value);
      }
    }

    // Namespaces that fall through without `continue` (resource with another owner) are untouched.
    if (!next.has(key)) next.set(key, value);
  }

  stats.values_rewritten = [...next.entries()].filter(([key, value]) => store[key] !== value).length;
  for (const peer of stats.trust_retained) {
    stats.warnings.push(`The trust row for ${peer} was kept, but it was recorded under ${from}; pair ${to} explicitly before ${peer} accepts it.`);
  }
  return { store: Object.fromEntries(next), stats };
}
