import { createHash } from 'node:crypto';
import {
  OnlyneNode, OnlyneError, MemoryStorage, type Auth, type MaintenanceClock, type NodeId,
  type KeyHook, type OperationRequest, type Plugins, type Resource, type ResourceRef, type Storage,
  type ThreadRef, type TraceEvent, type Transport,
} from '@onlyne/lib';
import { coordinateOf, createDriverBridge, resourceIdOf, type Connector, type ImEvent, type SendOutcome } from '@onlyne/im-core';
import { leveled, type LeveledLog, type LogLine, type LogSink } from '@onlyne/node-host';
import { mkdir, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { DEFAULT_WEBHOOK_PATH, type ChannelConfig, type GatewayConfig } from './config.js';
import { FileStorage } from './storage.js';
import { EventPusher } from './pusher.js';
import { ThreadRenderer, type RenderTarget } from './render.js';
import {
  connectorStatus, statusLines,
  type ChannelStatus, type GatewayStatusReport, type StatusCounters,
} from './status.js';
import { t } from './gateway-text.js';

/** A connector factory the gateway knows how to build from configuration alone. */
export type ConnectorFactory = (config: GatewayConfig, channel: ChannelConfig) => Promise<Connector>;

export type { ChannelConfig, ChannelTransport, GatewayConfig } from './config.js';
export { gatewayNodeId, DEFAULT_WEBHOOK_PATH } from './config.js';
export {
  statusLines,
  type ChannelStatus, type ConnectorStatus, type GatewayResourceStatus, type GatewayStatusReport, type StatusConnector,
} from './status.js';

/** The factory export each connector package carries; the gateway never imports a platform SDK. */
const CONNECTOR_EXPORTS: Record<string, string> = {
  fake: 'createFakeConnector', lark: 'createLarkConnector', qq: 'createQqConnector', wechat: 'createWechatConnector',
};

/** Connectors that report their own transitions, so the maintenance pass must not poll a second opinion. */
const STATE_REPORTING: Record<string, true> = { qq: true, lark: true };

/** At most one maintenance summary per minute: the pass count is what the operator reads, not every pass. */
const SUMMARY_WINDOW_MS = 60_000;

/**
 * The constructor argument each platform package expects. `im-fake` takes a script; a real
 * platform takes the v2 credential chain (explicit value, then credential file, then its
 * conventional environment pair, which the connector reads from `env`), plus the host log,
 * the inbound transport — with its callback path only when that transport is the webhook —
 * and the callback a connector reports its own state transitions to.
 */
function connectorOptions(
  config: GatewayConfig,
  channel: ChannelConfig,
  log: LogLine,
  onState: (to: string, detail?: Record<string, unknown> | Error) => void,
): unknown {
  if (channel.platform === 'fake') {
    const script = channel.script;
    const field = (name: 'events' | 'answers'): unknown[] => {
      if (typeof script !== 'object' || script === null) return [];
      const value = name === 'events' ? ('events' in script ? script.events : undefined) : ('answers' in script ? script.answers : undefined);
      return Array.isArray(value) ? value : [];
    };
    return { events: field('events'), answers: field('answers') };
  }
  const credentialFile = channel.credential_file === undefined
    ? undefined
    : channel.credential_file.startsWith('/') || config.credentials_dir === undefined
      ? channel.credential_file
      : join(config.credentials_dir, channel.credential_file);
  const webhookPath = channel.transport === 'websocket'
    ? undefined
    : channel.webhook_path ?? DEFAULT_WEBHOOK_PATH;
  return {
    ...(channel.credential === undefined ? {} : { credential: channel.credential }),
    ...(credentialFile === undefined ? {} : { credentialFile }),
    ...(config.media_dir === undefined ? {} : { mediaDir: config.media_dir }),
    ...(channel.transport === undefined ? {} : { transport: channel.transport }),
    ...(webhookPath === undefined ? {} : { webhookPath }),
    env: process.env,
    log,
    onState,
  };
}

/**
 * Loads one connector package on demand. A static import cannot work: a platform package is an
 * optional peer, so a host that runs one channel installs only that package. The specifier stays
 * runtime-resolved and a missing package answers `unsupported_delivery` instead of failing the
 * install of a host that serves another platform.
 */
async function builtinFactory(
  config: GatewayConfig,
  channel: ChannelConfig,
  log: LogLine,
  onState: (to: string, detail?: Record<string, unknown> | Error) => void,
): Promise<Connector> {
  const specifier: string = `@onlyne/im-${channel.platform}`;
  const mod: unknown = await import(specifier).catch(() => undefined);
  const name = CONNECTOR_EXPORTS[channel.platform] ?? 'createConnector';
  const create = typeof mod === 'object' && mod !== null && name in mod ? (mod as Record<string, unknown>)[name] : undefined;
  if (typeof create !== 'function') {
    throw new OnlyneError('unsupported_delivery', t('error.noConnectorPackage', { platform: channel.platform }));
  }
  return (create as (options: unknown) => Connector)(connectorOptions(config, channel, log, onState));
}

/**
 * A connector that receives platform events over HTTP instead of a socket or a poll loop. The
 * method is not on `Connector`: a connector without a callback stays a valid connector, and the
 * gateway routes only the one channel it serves.
 */
export interface WebhookConnector extends Connector {
  handleWebhook(request: Request): Promise<Response>;
}

export interface ImGateway {
  readonly node: OnlyneNode;
  readonly config: GatewayConfig;
  readonly storage: Storage;
  readonly pusher: EventPusher;
  /** The one platform this process serves. */
  readonly connector: Connector;
  /** Inbound path: one platform event enters the lib's dedup + pending queue, then the push fires. */
  receive(event: ImEvent): Promise<void>;
  /** The endpoint's HTTP surface, exactly the library's: adoption happens inside the node. */
  fetch(request: Request): Promise<Response>;
  /** The channel's inbound webhook (QQ), on the same port; a socket channel answers 404. */
  webhook(request: Request): Promise<Response>;
  /** One push-ring pass: every owed notify gets one attempt. */
  maintain(): Promise<void>;
  /** The host-side status view: platform state, counters, onboarding and the last failure. */
  status(): Promise<ChannelStatus>;
  /** The live readout a local listener answers: bindings, cursors and the owed-push count. */
  statusReport(): Promise<GatewayStatusReport>;
  /** The status view as display lines, for the node host's panel. */
  statusLines(): Promise<string[]>;
  stop(): Promise<void>;
}

export interface ImGatewayOptions {
  /** `memory` replaces the disk store; tests use it, a real gateway keeps the default. */
  storage?: 'memory' | 'disk';
  /** An overriding connector factory, keyed by platform. */
  factories?: Record<string, ConnectorFactory>;
  /** Replaces the HTTP transport; tests route peers in-process. */
  transport?: Transport;
  /**
   * RFC 0017: the node identity key. Supplied, the gateway signs every peer request, accepts
   * only signed traffic and declares `signature`; omitted, it runs the loopback development
   * binding that trusts a declared `Onlyne-Actor`. The CLI always supplies one.
   */
  keys?: KeyHook;
  /** The host's log. A logger from `createLogger` also carries the level entry points. */
  log?: LogSink;
}

/** `len` and `sha8` name a body in the log without putting it there. */
function digest(body: string): { len: number; sha8: string } {
  return { len: body.length, sha8: createHash('sha256').update(body, 'utf8').digest('hex').slice(0, 8) };
}

/** The library's source row for one platform event id (`key('resource', owner, id, 'source', source)`). */
const sourceRow = (ref: ResourceRef, source: string): string =>
  ['resource', ref.owner, ref.id, 'source', source].map(encodeURIComponent).join('/');

/** Where the host records the binding it last saw, so a handover becomes one line, not a snapshot. */
const bindingRow = (ref: ResourceRef): string =>
  `im/binding/${encodeURIComponent(ref.owner)}/${encodeURIComponent(ref.id)}`;

interface BindingRow { handler: NodeId; revision: string; cursor: string }

/** One conversation this endpoint serves, with the coordinate its id was derived from. */
interface ConversationResource { resource: Resource; conversation: string }

/**
 * RFC 0012 §1: the thread that mirrors one conversation, derived so a restart resolves the same one.
 */
const threadRefOf = (ref: ResourceRef): ThreadRef => ({ owner: ref.owner, id: `im-${ref.id}` });

/** The resource definition one conversation gets, whether it was adopted or pre-bound (RFC 0012 §3). */
function definitionOf(owner: NodeId, channel: ChannelConfig, ref: ResourceRef, conversation: string, connector: Connector): Resource {
  return {
    resource_ref: ref,
    kind: 'im_conversation',
    // The connector's own platform operations join the protocol's fixed four.
    operations: ['read', 'send', 'list_events', 'describe', 'handover', ...(connector.operations ?? [])],
    // RFC 0006 §3: the configured assignee is the one handler events route through. The
    // gateway keeps a standing log read for itself, which a handover never touches (§6).
    binding: { handler: channel.assignee ?? owner, cursor: '0' },
    log_readers: [owner],
    revision: 'rev-1',
    event_seq: '0',
    retention_ms: 3_600_000,
    data: { platform: channel.platform, conversation },
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

/** The operation result's own codes, so `platform.send` names the failure the caller is told. */
function platformOutcome(outcome: SendOutcome): { outcome: 'committed' | 'refused' | 'unknown'; code?: string } {
  if (outcome.state === 'committed' && outcome.error === undefined) return { outcome: 'committed' };
  if (outcome.state === 'committed') return { outcome: 'refused', code: 'capacity_exceeded' };
  return { outcome: 'unknown', code: 'unavailable' };
}

/**
 * Counts platform calls for the status view and records every outbound platform send, without
 * changing `Connector`: the bridge, the renderer and the pusher all keep talking to the
 * connector they were handed.
 */
function countedConnector(inner: Connector, counters: StatusCounters, out: LeveledLog): Connector {
  const record = async (work: () => Promise<SendOutcome>): Promise<SendOutcome> => {
    counters.sends += 1;
    counters.last_send_at = new Date().toISOString();
    try {
      const outcome = await work();
      if (outcome.error !== undefined) counters.send_failures += 1;
      return outcome;
    } catch (error) {
      counters.send_failures += 1;
      throw error;
    }
  };
  return {
    platform: inner.platform,
    start: () => inner.start(),
    onEvent: listener => inner.onEvent(listener),
    async send(request) {
      const body = digest(request.text);
      let outcome: SendOutcome;
      try {
        outcome = await record(() => inner.send(request));
      } catch (error) {
        const at = error instanceof OnlyneError ? error.error.code : undefined;
        out.warn('platform.send', {
          conversation: request.conversation, outcome: 'unknown',
          ...(at === undefined ? {} : { code: at }), ...body,
        });
        throw error;
      }
      const settled = platformOutcome(outcome);
      const fields = {
        conversation: request.conversation,
        ...settled,
        ...(outcome.message_id === undefined ? {} : { message_id: outcome.message_id }),
        ...body,
      };
      if (settled.outcome === 'committed') out.log('platform.send', fields);
      else out.warn('platform.send', fields);
      return outcome;
    },
    stop: () => inner.stop(),
    ...(inner.operations === undefined ? {} : { operations: inner.operations }),
    ...(inner.validate === undefined ? {} : { validate: (operation, data) => inner.validate!(operation, data) }),
    ...(inner.operate === undefined ? {} : { operate: (operation, data) => record(() => inner.operate!(operation, data)) }),
  };
}

function hasWebhook(connector: Connector): connector is WebhookConnector {
  return typeof (connector as Partial<WebhookConnector>).handleWebhook === 'function';
}

/**
 * The reconcile failure the library reports once per `(owner, thread, code)` window. Declared
 * beside the library's own trace union because the host is the side that renders it.
 */
export interface ReconcileFailureTrace {
  event: 'reconcile.failed';
  owner: NodeId;
  thread: string;
  code: string;
  message: string;
  attempts: number;
}

/** Everything the host renders from the library's trace. */
export type HostTrace = TraceEvent | ReconcileFailureTrace;

/** One log line for a trace the host renders itself, or `undefined` when the line is not its job. */
export interface TraceLine {
  level: 'debug' | 'warn';
  message: 'reconcile.failed' | 'inbound.acked';
  fields: Record<string, unknown>;
}

/**
 * The two traces the host renders: a failed reconcile pull is a degraded node, so it is a warning
 * with the reason and the number of failures the library collapsed into it; an ack that closes an
 * obligation is per-pass detail.
 */
export function traceLine(event: HostTrace): TraceLine | undefined {
  if (event.event === 'reconcile.failed') {
    return {
      level: 'warn',
      message: 'reconcile.failed',
      fields: {
        owner: event.owner, thread: event.thread, code: event.code, message: event.message, attempts: event.attempts,
      },
    };
  }
  if (event.event === 'inbound.acked') {
    return {
      level: 'debug',
      message: 'inbound.acked',
      fields: { owner: event.thread.owner, thread: event.thread.id, seq: event.seq, obligation: event.obligation },
    };
  }
  return undefined;
}

interface StateReporter {
  /** A transition the connector reported itself, carrying the shard and session it held. */
  report(to: string, fields?: Record<string, unknown>): void;
  /** A state read from `status()`, for a connector that cannot report its own transitions. */
  observe(state: string, error?: string): void;
}

/**
 * One line per connector state transition. RFC 0012 §4 keeps the connector a translation layer,
 * so the host owns the line: a connector that reports its own transitions (qq, lark) feeds this
 * with the shard and session it held, and anything else is observed from `status()` once per
 * maintenance pass, so a connector that cannot report still shows its transitions.
 */
function stateReporter(platform: string, out: LeveledLog): StateReporter {
  let last: string | undefined;
  const report = (to: string, fields: Record<string, unknown> = {}): void => {
    if (last === to) return;
    // A connector that reports nothing yet is not transitioning to `unknown`; the first real
    // state is what the operator needs to see.
    if (last === undefined && to === 'unknown') return;
    out.log('connector.state', { platform, from: last ?? 'unknown', to, ...fields });
    last = to;
  };
  return { report, observe: (state, error) => report(state, error === undefined ? {} : { error }) };
}

/**
 * Assembles the gateway: bridge first (the library captures the driver at service
 * construction, resources.ts:159, so the resource accessor is lazy), then the node.
 */
export async function createImGateway(config: GatewayConfig, options: ImGatewayOptions = {}): Promise<ImGateway> {
  const out = leveled(options.log);
  const log = out.log;
  const storage: Storage = options.storage === 'memory' ? new MemoryStorage() : new FileStorage(config.storage_path);
  const owner: NodeId = config.node.id;
  const channel = config.channel;
  const counters: StatusCounters = { events: 0, sends: 0, send_failures: 0 };
  const startedAt = Date.now();
  const states = stateReporter(channel.platform, out);
  /** qq and lark carry the same callback shape to their connectors; the second argument is a field set or an error. */
  const reportState = (to: string, detail?: Record<string, unknown> | Error): void => {
    states.report(to, detail instanceof Error ? { error: detail.message } : detail);
  };
  let startFailure: string | undefined;

  // The connector comes first: the bridge pins its platform table at construction, while the
  // resource service stays a lazy accessor because the library captures the driver when
  // the service is built (resources.ts:159).
  const builtin = options.factories?.[channel.platform] === undefined;
  const factory = options.factories?.[channel.platform]
    ?? ((cfg: GatewayConfig, entry: ChannelConfig) => builtinFactory(cfg, entry, log, reportState));
  const platformConnector = await factory(config, channel);
  if (platformConnector.platform !== channel.platform) {
    throw new OnlyneError('invalid_request', t('error.noConnectorPlatform', { platform: channel.platform, event: platformConnector.platform }));
  }
  const connector = countedConnector(platformConnector, counters, out);
  const rememberBinding = async (resource: Resource): Promise<void> => {
    await storage.transaction(async tx => {
      await tx.put(bindingRow(resource.resource_ref), {
        handler: resource.binding.handler, revision: resource.revision, cursor: resource.binding.cursor,
      } satisfies BindingRow);
    });
  };
  /**
   * One line per binding change this pass observes. The library commits a handover inside its
   * own service, so the host compares the binding it last recorded with the live one; a cursor
   * that moved on its own (an ack) updates the record without being a binding transition.
   */
  const watchBindings = async (entries: ConversationResource[]): Promise<void> => {
    for (const { resource } of entries) {
      const key = bindingRow(resource.resource_ref);
      const current: BindingRow = {
        handler: resource.binding.handler, revision: resource.revision, cursor: resource.binding.cursor,
      };
      const prior = await storage.transaction(async tx => tx.get<BindingRow>(key));
      if (prior === undefined || (prior.handler === current.handler && prior.revision === current.revision && prior.cursor === current.cursor)) {
        if (prior === undefined) await rememberBinding(resource);
        continue;
      }
      if (prior.handler !== current.handler) {
        out.log('resource.binding', {
          action: 'handover', resource: `${resource.resource_ref.owner}/${resource.resource_ref.id}`,
          old_handler: prior.handler, new_handler: current.handler,
          revision: current.revision, cursor: current.cursor,
        });
      }
      await rememberBinding(resource);
    }
  };
  const adopt = async (ref: ResourceRef, conversation: string): Promise<void> => {
    const coordinate = coordinateOf(ref.id);
    if (coordinate === undefined || coordinate.platform !== channel.platform) {
      throw new OnlyneError('invalid_request', t('error.noConnectorResource', { resource: ref.id }));
    }
    const existing = await node.resources.describe(node.auth, ref).then(resource => resource, (error: unknown) => {
      if (error instanceof OnlyneError && error.error.code === 'not_found') return undefined;
      throw error;
    });
    const resource = await node.resources.register(node.auth, definitionOf(owner, channel, ref, conversation, connector));
    await ensureThread(node, threadRefOf(ref), conversation);
    if (existing !== undefined) return;
    out.log('resource.binding', {
      action: 'bind', resource: `${ref.owner}/${ref.id}`,
      old_handler: null, new_handler: resource.binding.handler,
      revision: resource.revision, cursor: resource.binding.cursor,
    });
    await rememberBinding(resource);
  };
  const bridge = createDriverBridge({
    node: owner,
    resources: () => node.resources,
    storage: { transaction: work => storage.transaction(work) },
    connectors: [{ platform: channel.platform, connector }],
    adopt,
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
  let node: OnlyneNode;
  const base_url = `http://${config.server.bind}:${config.server.port}/onlyne/v3`;
  const resolve = async (id: NodeId) => {
    if (id === owner) return { node_id: id, base_url };
    const target = await node.discovery.resolve(id);
    return { ...target, headers: { 'Onlyne-Actor': owner } };
  };
  // The keyless development binding: an HTTP caller names itself. A keyed node never reaches
  // this hook — the library verifies the request signature and the actor comes from the key.
  const authenticate = async (request: Request): Promise<Auth | null> => {
    const actor = request.headers.get('Onlyne-Actor');
    if (actor === null || actor.length === 0) return null;
    if (actor === owner) throw new OnlyneError('forbidden', t('error.claimIdentity'));
    return { actor };
  };
  /**
   * RFC 0012 §5: a send may name a conversation this endpoint has not served yet, and the
   * library answers `not_found` before the driver runs. The hook sits after the library
   * authenticated the caller and parsed the operation, so an anonymous request never creates a
   * resource, and `bridge.resolve` runs the same adopt a first inbound event runs.
   */
  const prepareOperation = async (_auth: Auth, ref: ResourceRef, request: OperationRequest): Promise<void> => {
    if (request.operation !== 'send' || ref.owner !== owner) return;
    const coordinate = coordinateOf(ref.id);
    if (coordinate === undefined || coordinate.platform !== channel.platform) return;
    await bridge.resolve(coordinate.platform, coordinate.conversation);
  };
  const plugins: Omit<Plugins, 'storage' | 'resources'> = {
    authenticate,
    maintenanceClock: nodeClock(),
    resolve,
    prepareResourceOperation: prepareOperation,
    domainCandidates: domain_id => node.discovery.candidates(domain_id),
    ...(options.keys === undefined ? {} : { keys: options.keys }),
    decide: () => { throw new OnlyneError('decision_unavailable', t('error.decide')); },
    runtime: {
      // RFC 0017 §5: a trust change is a human decision taken at the terminal the host runs,
      // so the gateway reports the peer, its stage and the fingerprint the holder compares.
      notify: event => {
        if (event.event !== 'trust') return;
        log('im.trust', { node: event.entry.node_id, phase: event.phase, fingerprint: event.entry.fingerprint });
      },
      // Only `await` needs a model context to hold; a notify lands in the gateway's own
      // ledger, and the gateway holds no context to await a reply in.
      supports: async plan => plan.response !== 'await',
      inject: async () => { throw new OnlyneError('unsupported_delivery', t('error.noContext')); },
      // The library reports each lifecycle transition once: a reconcile failure that keeps its
      // rows is a degraded node, and an ack that closes an obligation is a per-pass detail.
      trace: (event: HostTrace) => {
        const line = traceLine(event);
        if (line !== undefined) out[line.level](line.message, line.fields);
      },
    },
  };
  node = new OnlyneNode({
    node_id: owner,
    base_url,
    ...(options.keys === undefined ? {} : { authentication: ['signature'] }),
    plugins: {
      ...plugins, storage, resources: bridge,
      ...(options.transport === undefined ? {} : { transport: options.transport }),
    },
    now: () => Date.now(),
    id: () => crypto.randomUUID(),
  });

  const pusher = new EventPusher({
    client: () => node.client,
    storage,
    sender: owner,
    // RFC 0012 §1: one thread per conversation, its id derived from the resource.
    threadOf: threadRefOf,
    // An event names no recipient (RFC 0006 §5), so the push asks the binding each time and a
    // handover moves the target with it.
    handlerOf: async ref => (await node.resources.describe(node.auth, ref)).binding.handler,
    log: out,
  });
  /**
   * Every conversation this endpoint serves, read from the resources themselves: a conversation
   * adopted after startup must render its replies without the renderer being told about it.
   */
  const conversationResources = async (): Promise<ConversationResource[]> => {
    const resources: ConversationResource[] = [];
    let cursor: string | undefined;
    for (;;) {
      const page = await node.resources.list(node.auth, { page_size: 100, ...(cursor === undefined ? {} : { cursor }) });
      for (const resource of page.items) {
        const coordinate = coordinateOf(resource.resource_ref.id);
        if (coordinate === undefined || coordinate.platform !== channel.platform) continue;
        resources.push({ resource, conversation: coordinate.conversation });
      }
      if (page.next_cursor === null) return resources;
      cursor = page.next_cursor;
    }
  };
  const targetsOf = (entries: ConversationResource[]): RenderTarget[] => entries.map(({ resource, conversation }) => ({
    ref: resource.resource_ref,
    thread: threadRefOf(resource.resource_ref),
    connector,
    conversation,
  }));
  const renderer = new ThreadRenderer({ node, storage, bridge, log: out });
  let summaryAt = 0;
  let summaryPasses = 0;
  let summaryReplayed = 0;
  let summaryFailures = 0;
  const gateway: ImGateway = {
    node, config, storage, pusher, connector,
    async receive(event: ImEvent): Promise<void> {
      if (event.platform !== channel.platform) {
        throw new OnlyneError('unsupported_delivery', t('error.noConnectorPlatform', { platform: channel.platform, event: event.platform }));
      }
      // A duplicate `source_id` reuses the stored event; the push is idempotent by
      // message identity, and the owed row is keyed by event id, so this is safe.
      // The library's source row is the deduplication point, so reading it names the
      // disposition without a second dedupe rule or a changed service signature.
      const ref: ResourceRef = { owner, id: resourceIdOf(event.platform, event.conversation) };
      const seen = await storage.transaction(async tx => tx.get<string>(sourceRow(ref, event.source_id)));
      const stored = await bridge.receive(event);
      counters.events += 1;
      counters.last_event_at = event.at;
      log('inbound.event', {
        conversation: event.conversation, kind: event.kind, source_id: event.source_id,
        event_seq: stored.event_id, dedupe: seen === undefined ? 'new' : 'duplicate', ...digest(event.text),
      });
      await pusher.afterReceive(stored.resource_ref, stored);
    },
    async fetch(request: Request): Promise<Response> {
      return node.fetch(request);
    },
    async webhook(request: Request): Promise<Response> {
      // The gateway socket and the signed callback are alternative receive paths: serving both
      // would deliver every event twice, so the callback answers 404 while the socket is in force.
      if (channel.transport === 'websocket' || !hasWebhook(platformConnector)) {
        return Response.json(
          { error: { code: 'not_found', message: t('error.noWebhook', { platform: channel.platform }) } },
          { status: 404 },
        );
      }
      return platformConnector.handleWebhook(request);
    },
    async maintain(): Promise<void> {
      const before = pusher.stats();
      const resources = await conversationResources();
      await watchBindings(resources);
      if (!(builtin && STATE_REPORTING[channel.platform] === true)) {
        const observed = connectorStatus(platformConnector);
        states.observe(observed.state, observed.error);
      }
      await pusher.retryOwed();
      await renderer.pass(targetsOf(resources));
      const after = pusher.stats();
      summaryPasses += 1;
      summaryReplayed += after.attempts - before.attempts;
      summaryFailures += after.failures - before.failures;
      const now = Date.now();
      if (summaryAt === 0 || now - summaryAt >= SUMMARY_WINDOW_MS) {
        log('gateway.maintain.summary', { passes: summaryPasses, replayed: summaryReplayed, failures: summaryFailures });
        summaryAt = now;
        summaryPasses = 0;
        summaryReplayed = 0;
        summaryFailures = 0;
      }
    },
    async status(): Promise<ChannelStatus> {
      const observed = connectorStatus(platformConnector);
      return {
        node: owner,
        platform: channel.platform,
        state: observed.state,
        connected: observed.connected,
        reconnects: observed.reconnects,
        rate_limits: observed.rate_limits,
        ...(observed.transport === undefined ? {} : { transport: observed.transport }),
        ...(observed.heartbeat_timeouts === undefined ? {} : { heartbeat_timeouts: observed.heartbeat_timeouts }),
        ...(observed.resumes === undefined ? {} : { resumes: observed.resumes }),
        ...(observed.resume_failures === undefined ? {} : { resume_failures: observed.resume_failures }),
        ...(counters.last_event_at === undefined && observed.last_event_at === undefined
          ? {}
          : { last_event_at: counters.last_event_at ?? observed.last_event_at }),
        ...(observed.onboarding === undefined ? {} : { onboarding: observed.onboarding }),
        error: observed.error ?? startFailure,
        events: counters.events,
        sends: counters.sends,
        send_failures: counters.send_failures,
        ...(counters.last_send_at === undefined ? {} : { last_send_at: counters.last_send_at }),
        uptime_ms: Date.now() - startedAt,
      };
    },
    async statusReport(): Promise<GatewayStatusReport> {
      const status = await gateway.status();
      const resources = await conversationResources();
      const owed = (await storage.transaction(async tx => tx.scan('im/push/'))).length;
      return {
        node: status.node,
        platform: status.platform,
        transport: status.transport ?? channel.transport ?? 'platform',
        connector: {
          state: status.state,
          connected: status.connected,
          reconnects: status.reconnects,
          rate_limits: status.rate_limits,
          ...(status.onboarding === undefined ? {} : { onboarding: status.onboarding }),
          ...(status.error === undefined ? {} : { error: status.error }),
        },
        counters: {
          events: status.events,
          sends: status.sends,
          send_failures: status.send_failures,
          uptime_ms: status.uptime_ms,
          ...(status.last_event_at === undefined ? {} : { last_event_at: status.last_event_at }),
          ...(status.last_send_at === undefined ? {} : { last_send_at: status.last_send_at }),
        },
        resources: resources.map(({ resource, conversation }) => ({
          resource: `${resource.resource_ref.owner}/${resource.resource_ref.id}`,
          platform: channel.platform,
          conversation,
          handler: resource.binding.handler,
          cursor: resource.binding.cursor,
          revision: resource.revision,
        })),
        owed,
      };
    },
    async statusLines(): Promise<string[]> {
      return statusLines(await gateway.status());
    },
    async stop(): Promise<void> {
      node.client.stopMaintenance();
      await platformConnector.stop();
    },
  };

  // RFC 0012 §2: a configured conversation is a pre-binding — the same adopt, run at startup.
  if (channel.conversation !== undefined) await bridge.resolve(channel.platform, channel.conversation);
  // The returned promise is what serializes events per conversation: the connector awaits it
  // before the next event of the same chat, so `receive` calls land in arrival order.
  connector.onEvent(event =>
    gateway.receive(event).catch(failure => {
      // The platform event reached the connector but this endpoint lost it, so the line is an
      // error rather than a degraded state.
      out.error('im.receive.failed', { platform: event.platform, source: event.source_id, error: String(failure) });
    }));
  try {
    await platformConnector.start();
  } catch (error) {
    // A channel that cannot start is an operator-visible state, not a reason to kill the node:
    // its status and log carry the failure, and a credential fix reconnects on the next start.
    startFailure = error instanceof Error ? error.message : String(error);
    out.warn('im.connector.start.failed', { platform: channel.platform, error: startFailure });
  }

  for (const seed of config.peers ?? []) {
    try { await node.discovery.observe(seed, 'bootstrap'); } catch (error) { out.warn('im.discovery.failed', { seed, error: String(error) }); }
  }
  // Static only: node, endpoint, platform, transport and store. Everything that moves is read
  // from `statusReport()` or the TUI, never from a line written once at startup.
  log('im.gateway.listening', {
    node: owner, endpoint: base_url, platform: channel.platform,
    transport: channel.transport ?? 'platform', storage: config.storage_path,
  });
  return gateway;
}

/** The Node-timer MaintenanceClock: one-shot timers, the lib's loop re-arms them. */
export function nodeClock(): MaintenanceClock {
  return {
    setTimer: (ms, run) => setTimeout(run, ms),
    clearTimer: handle => { clearTimeout(handle as ReturnType<typeof setTimeout>); },
  };
}
