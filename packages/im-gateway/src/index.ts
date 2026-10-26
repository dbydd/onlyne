import {
  OnlyneNode, OnlyneError, MemoryStorage, type Auth, type Json, type MaintenanceClock, type NodeId,
  type Plugins, type ResourceRef, type ResourceEvent, type Storage, type ThreadRef, type Transport,
} from '@onlyne/lib';
import { createDriverBridge, type Connector, type ImEvent } from '@onlyne/im-core';
import { mkdir, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import type { ConnectorConfig, GatewayConfig } from './config.js';
import { FileStorage } from './storage.js';
import { EventPusher } from './pusher.js';
import { ThreadRenderer, type RenderTarget } from './render.js';

/** A connector factory the gateway knows how to build from configuration alone. */
export type ConnectorFactory = (config: GatewayConfig, entry: ConnectorConfig) => Promise<Connector>;

export type { ConnectorConfig, GatewayConfig } from './config.js';

/** The factory export each connector package carries; the gateway never imports a platform SDK. */
const CONNECTOR_EXPORTS: Record<string, string> = { fake: 'createFakeConnector', lark: 'createLarkConnector' };

/**
 * The constructor argument each platform package expects. `im-fake` takes a script; a real
 * platform takes the v2 credential chain (explicit value, then credential file, then its
 * conventional environment pair, which the connector reads from `env`).
 */
function connectorOptions(config: GatewayConfig, entry: ConnectorConfig): unknown {
  if (entry.platform === 'fake') {
    const script = entry.script;
    const field = (name: 'events' | 'answers'): unknown[] => {
      if (typeof script !== 'object' || script === null) return [];
      const value = name === 'events' ? ('events' in script ? script.events : undefined) : ('answers' in script ? script.answers : undefined);
      return Array.isArray(value) ? value : [];
    };
    return { events: field('events'), answers: field('answers') };
  }
  const credentialFile = entry.credential_file === undefined
    ? undefined
    : entry.credential_file.startsWith('/') || config.credentials_dir === undefined
      ? entry.credential_file
      : join(config.credentials_dir, entry.credential_file);
  return {
    ...(entry.credential === undefined ? {} : { credential: entry.credential }),
    ...(credentialFile === undefined ? {} : { credentialFile }),
    ...(config.media_dir === undefined ? {} : { mediaDir: config.media_dir }),
    env: process.env,
  };
}

/**
 * Loads one connector package on demand. `im-lark` is an optional peer: a host that runs
 * only some platforms installs only those packages, so the specifier stays runtime-resolved
 * and a missing package answers `unsupported_delivery` instead of failing the install.
 */
async function builtinFactory(config: GatewayConfig, entry: ConnectorConfig): Promise<Connector> {
  const specifier: string = `@onlyne/im-${entry.platform}`;
  const mod: unknown = await import(specifier).catch(() => undefined);
  const name = CONNECTOR_EXPORTS[entry.platform] ?? 'createConnector';
  const create = typeof mod === 'object' && mod !== null && name in mod ? (mod as Record<string, unknown>)[name] : undefined;
  if (typeof create !== 'function') {
    throw new OnlyneError('unsupported_delivery', `No connector package serves platform ${entry.platform}.`);
  }
  return (create as (options: unknown) => Connector)(connectorOptions(config, entry));
}

export interface ImGateway {
  readonly node: OnlyneNode;
  readonly config: GatewayConfig;
  readonly storage: Storage;
  readonly pusher: EventPusher;
  readonly connectors: Connector[];
  /** Inbound path: one platform event enters the lib's dedup + pending queue, then the push fires. */
  receive(event: ImEvent): Promise<void>;
  /** One push-ring pass: every owed notify gets one attempt. */
  maintain(): Promise<void>;
  stop(): Promise<void>;
}

export interface ImGatewayOptions {
  /** `memory` replaces the disk store; tests use it, a real gateway keeps the default. */
  storage?: 'memory' | 'disk';
  /** Extra or overriding connector factories, keyed by platform. */
  factories?: Record<string, ConnectorFactory>;
  /** Replaces the HTTP transport; tests route peers in-process. */
  transport?: Transport;
  log?: (message: string, fields?: Record<string, unknown>) => void;
}

/** The lib's key encoding (core.ts:25) and layout for one resource (resources.ts:44-47). */
const key = (...parts: string[]) => parts.map(encodeURIComponent).join('/');
const resourceKey = (ref: ResourceRef, tail: string[]) => key('resource', ref.owner, ref.id, ...tail);

/**
 * The event a `receive` just stored. `bridge.receive` does not return it, and the gateway
 * owns the storage the lib wrote to, so the source row is read directly instead of
 * diffing the whole event list.
 */
async function storedEvent(storage: Storage, ref: ResourceRef, sourceId: string): Promise<ResourceEvent | undefined> {
  const eventId = await storage.transaction(async tx => tx.get<string>(resourceKey(ref, ['source', sourceId])));
  if (eventId === undefined) return undefined;
  return storage.transaction(async tx => tx.get<ResourceEvent>(resourceKey(ref, ['event', eventId])));
}

/** The resource definition the gateway registers for one configured connector. */
function definitionOf(config: GatewayConfig, entry: ConnectorConfig, connector: Connector): {
  resource_ref: ResourceRef;
  kind: 'im_conversation';
  operations: string[];
  assignee?: NodeId;
  revision: string;
  readers: NodeId[];
  controllers: NodeId[];
  event_recipients: NodeId[];
  result_recipients: NodeId[];
  event_seq: string;
  retention_ms: number;
  data: Json;
} {
  const owner = config.node.id;
  const ref: ResourceRef = { owner: entry.resource_ref.owner ?? owner, id: entry.resource_ref.id };
  const assignee = entry.assignee;
  return {
    resource_ref: ref,
    kind: 'im_conversation',
    // The connector's own platform operations join the protocol's fixed four.
    operations: ['read', 'send', 'list_events', 'describe', 'handover', ...(connector.operations ?? [])],
    ...(assignee === undefined ? {} : { assignee }),
    revision: 'rev-1',
    readers: [owner],
    controllers: [],
    event_recipients: assignee === undefined ? [owner] : [assignee],
    result_recipients: assignee === undefined ? [owner] : [assignee],
    event_seq: '0',
    retention_ms: 3_600_000,
    data: { platform: entry.platform, conversation: entry.conversation },
  };
}

/**
 * RFC 0012 §1: the gateway owns one thread per platform conversation. The id is derived from
 * the resource, so a restart resolves the same thread instead of opening a second one.
 */
async function ensureThread(node: OnlyneNode, ref: ThreadRef, title: string): Promise<void> {
  try {
    await node.threads.describe(node.auth, ref);
    return;
  } catch (error) {
    if (!(error instanceof OnlyneError) || error.error.code !== 'not_found') throw error;
  }
  try {
    await node.threads.create(node.auth, {
      idempotency_key: crypto.randomUUID(),
      retry_until: new Date(Date.now() + 60_000).toISOString(),
      title,
      id: ref.id,
    });
  } catch (error) {
    // A concurrent registration may have created it first; only that conflict is benign.
    if (!(error instanceof OnlyneError) || error.error.code !== 'id_conflict') throw error;
  }
}

/**
 * Assembles the gateway: bridge first (the library captures the driver at service
 * construction, resources.ts:159, so the resource accessor is lazy), then the node.
 */
export async function createImGateway(config: GatewayConfig, options: ImGatewayOptions = {}): Promise<ImGateway> {
  const log = options.log ?? (() => undefined);
  const storage: Storage = options.storage === 'memory' ? new MemoryStorage() : new FileStorage(config.storage_path);
  const owner: NodeId = config.node.id;

  let node: OnlyneNode;
  // Connectors come first: the bridge pins its platform table at construction, while the
  // resource service stays a lazy accessor because the library captures the driver when
  // the service is built (resources.ts:159).
  const connectorRefs: Array<{ ref: ResourceRef; connector: Connector }> = [];
  for (const entry of config.connectors) {
    const factory = options.factories?.[entry.platform] ?? builtinFactory;
    connectorRefs.push({
      ref: { owner: entry.resource_ref.owner ?? owner, id: entry.resource_ref.id },
      connector: await factory(config, entry),
    });
  }
  const bridge = createDriverBridge({
    node: owner,
    resources: () => node.resources,
    storage: { transaction: work => storage.transaction(work) },
    connectors: connectorRefs,
    writeAttachment: async (ref, name, bytes) => {
      // Inline bytes become a file the connector can upload. The workspace is `media_dir` when
      // configured, otherwise a sibling of the store, so a host without one still accepts them.
      const root = config.media_dir ?? join(dirname(config.storage_path), 'media');
      const safe = name.replace(/[^A-Za-z0-9._-]/g, '_').replace(/^\.+/, '') || 'file';
      const directory = join(root, 'outbound', ref.id.replace(/[^A-Za-z0-9._-]/g, '_'));
      await mkdir(directory, { recursive: true });
      const path = join(directory, `${crypto.randomUUID()}-${safe}`);
      await writeFile(path, bytes);
      return path;
    },
  });
  const base_url = `http://${config.server.bind}:${config.server.port}/onlyne/v3`;
  const resolve = async (id: NodeId) => {
    if (id === owner) return { node_id: id, base_url };
    const target = await node.discovery.resolve(id);
    return { ...target, headers: { 'Onlyne-Actor': owner } };
  };
  // HTTP callers name themselves; RFC 0017 keys are not wired yet, so this is the
  // unauthenticated development binding, and the gateway's own identity stays off-limits.
  const authenticate = async (request: Request): Promise<Auth | null> => {
    const actor = request.headers.get('Onlyne-Actor');
    if (actor === null || actor.length === 0) return null;
    if (actor === owner) throw new OnlyneError('forbidden', 'HTTP callers cannot claim the gateway identity.');
    return { actor };
  };
  const plugins: Omit<Plugins, 'storage' | 'resources'> = {
    authenticate,
    maintenanceClock: nodeClock(),
    resolve,
    decide: () => { throw new OnlyneError('decision_unavailable', 'The gateway forwards messages; it does not decide sends.'); },
    runtime: {
      notify: () => undefined,
      // Only `await` needs a model context to hold; a notify lands in the gateway's own
      // ledger, and the gateway holds no context to await a reply in.
      supports: async plan => plan.response !== 'await',
      inject: async () => { throw new OnlyneError('unsupported_delivery', 'The gateway consumes no model context.'); },
    },
  };
  node = new OnlyneNode({
    node_id: owner,
    base_url,
    plugins: {
      ...plugins, storage, resources: bridge,
      ...(options.transport === undefined ? {} : { transport: options.transport }),
    },
    now: () => Date.now(),
    id: () => crypto.randomUUID(),
  });

  // One thread per platform conversation (RFC 0012 §1), keyed by the resource it serves.
  const conversationThreads = new Map<string, ThreadRef>();
  const pusher = new EventPusher({
    client: () => node.client,
    storage,
    sender: owner,
    threadOf: ref => conversationThreads.get(`${ref.owner}/${ref.id}`),
    log,
  });
  const renderTargets: RenderTarget[] = [];
  const renderer = new ThreadRenderer({ node, storage, bridge, log });
  const gateway: ImGateway = {
    node, config, storage, pusher, connectors: [],
    async receive(event: ImEvent): Promise<void> {
      const serving = connectorRefs.find(candidate => candidate.connector.platform === event.platform);
      if (serving === undefined) {
        throw new OnlyneError('unsupported_delivery', `No connector serves platform ${event.platform}.`);
      }
      await bridge.receive(event);
      // A duplicate `source_id` reuses the stored event; the push is idempotent by
      // message identity, and the owed row is keyed by event id, so this is safe.
      const stored = await storedEvent(storage, serving.ref, event.source_id);
      if (stored !== undefined) await pusher.afterReceive(serving.ref, stored);
    },
    async maintain(): Promise<void> {
      await pusher.retryOwed();
      await renderer.pass(renderTargets);
    },
    async stop(): Promise<void> {
      node.client.stopMaintenance();
      for (const connector of gateway.connectors) await connector.stop();
    },
  };

  for (const [index, entry] of config.connectors.entries()) {
    const serving = connectorRefs[index]!;
    gateway.connectors.push(serving.connector);
    try {
      await node.resources.describe(node.auth, serving.ref);
    } catch (error) {
      if (!(error instanceof OnlyneError) || error.error.code !== 'not_found') throw error;
      await node.resources.register(node.auth, definitionOf(config, entry, serving.connector));
    }
    // The thread exists before the first event arrives, so the push always has a landing.
    const threadRef: ThreadRef = { owner, id: `im-${entry.resource_ref.id}` };
    await ensureThread(node, threadRef, entry.conversation);
    conversationThreads.set(`${serving.ref.owner}/${serving.ref.id}`, threadRef);
    renderTargets.push({ ref: serving.ref, thread: threadRef, connector: serving.connector, conversation: entry.conversation });
    // The returned promise is what serializes events per conversation: the connector awaits it
    // before the next event of the same chat, so `receive` calls land in arrival order.
    serving.connector.onEvent(event =>
      gateway.receive(event).catch(failure => {
        log('im.receive.failed', { platform: event.platform, source: event.source_id, error: String(failure) });
      }));
    await serving.connector.start();
  }

  for (const seed of config.peers ?? []) {
    try { await node.discovery.observe(seed, 'bootstrap'); } catch (error) { log('im.discovery.failed', { seed, error: String(error) }); }
  }
  return gateway;
}

/** The Node-timer MaintenanceClock: one-shot timers, the lib's loop re-arms them. */
export function nodeClock(): MaintenanceClock {
  return {
    setTimer: (ms, run) => setTimeout(run, ms),
    clearTimer: handle => { clearTimeout(handle as ReturnType<typeof setTimeout>); },
  };
}
