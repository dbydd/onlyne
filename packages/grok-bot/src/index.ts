import {
  OnlyneError, MemoryStorage, OnlyneNode, type Auth, type DeliveryPlan, type Entry, type Json,
  type KeyHook, type MaintenanceClock, type Message, type MessageRef, type NodeId, type Plugins,
  type Storage, type ThreadRef, type Transport,
} from '@onlyne/lib';
import { FileStorage } from '@onlyne/im-gateway/storage';
import { leveled, type LogSink } from '@onlyne/node-host';
import { timingSafeEqual } from 'node:crypto';
import { DEFAULT_STATUS_PATH, redactWebhook, webhookHeaderValue, type GrokBotConfig } from './config.ts';
import { t } from './node-text.ts';
import { clampLeaseMs, clampLimit, clampWaitMs, PullInbox, type PullStats } from './pull.ts';
import { installGrokBotAdmit } from './threads.ts';
import { enqueueNotify, WebhookDispatcher, type FetchLike } from './webhook.ts';

export type { GrokBotConfig, WebhookConfig } from './config.ts';
export {
  DEFAULT_ACK_PATH, DEFAULT_EVENTS_PATH, DEFAULT_HEADER, DEFAULT_MAX_ATTEMPTS, DEFAULT_NODE_ID,
  DEFAULT_REPLY_PATH, DEFAULT_RETRY_BASE_MS, DEFAULT_SCHEME, DEFAULT_STATUS_PATH, DEFAULT_TIMEOUT_MS,
  expandEnv, loadConfig, redactWebhook, webhookHeaderValue, parseTrustSeeds,
} from './config.ts';
export { deliveryId, urlForLog, webhookPayload, WebhookDispatcher, type FetchLike } from './webhook.ts';
export {
  GrokBotPullClient, PullInbox, clampLeaseMs, clampLimit, clampWaitMs,
  DEFAULT_PULL_LEASE_MS, DEFAULT_PULL_LIMIT, DEFAULT_PULL_WAIT_MS, MAX_PULL_WAIT_MS,
  SERVE_IDLE_TIMEOUT_SEC,
  type GrokBotPullClientOptions, type GrokBotReplyBody, type PullItem, type PullRow, type PullStats,
} from './pull.ts';
export {
  bindGrokBotInbound, grokBotChatFromMessage, grokBotChatFromThread, grokBotThreadId,
  parseGrokBotThreadId, GROK_BOT_DM_PREFIX, GROK_BOT_GROUP_PREFIX,
  type GrokBotChat, type GrokBotChatKind,
} from './threads.ts';

export { DEFAULT_STATUS_PATH as STATUS_PATH } from './config.ts';

/** The Node-timer MaintenanceClock: one-shot timers, the lib's loop re-arms them. */
export function nodeClock(): MaintenanceClock {
  return {
    setTimer: (ms, run) => setTimeout(run, ms),
    clearTimer: handle => clearTimeout(handle as ReturnType<typeof setTimeout>),
  };
}

export interface GrokBotStatus {
  node: NodeId;
  webhook: ReturnType<typeof redactWebhook>;
  queue: { owed: number; failed: number; delivered: number; attempts: number; in_flight: number; rows: number };
  pull: PullStats;
  reply_path: string;
  events_path: string;
}

export interface GrokBotNode {
  readonly node: OnlyneNode;
  readonly config: GrokBotConfig;
  readonly storage: Storage;
  readonly dispatcher: WebhookDispatcher;
  readonly pull: PullInbox;
  /** The endpoint's HTTP surface: reply, events, status first, then the library. */
  fetch(request: Request): Promise<Response>;
  status(): Promise<GrokBotStatus>;
  statusLines(): Promise<string[]>;
  /** One retry pass over owed webhook rows. */
  maintain(): Promise<void>;
  /**
   * Approve configured `trust_seeds` whose live `/keys` fingerprint matches the pin.
   * A mismatch is refused; peers not in the list are untouched.
   */
  approvePinned(): Promise<void>;
  stop(): Promise<void>;
}

export interface GrokBotOptions {
  storage?: 'memory' | 'disk';
  transport?: Transport;
  keys?: KeyHook;
  log?: LogSink;
  fetch?: FetchLike;
  now?: () => number;
}

function headerEqual(left: string, right: string): boolean {
  const a = Buffer.from(left);
  const b = Buffer.from(right);
  if (a.length !== b.length) return false;
  return timingSafeEqual(a, b);
}

const rec = (value: unknown): Record<string, unknown> | undefined =>
  value !== null && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : undefined;

function threadRef(value: unknown): ThreadRef | undefined {
  const object = rec(value);
  if (object === undefined) return undefined;
  if (typeof object.owner !== 'string' || object.owner.length === 0) return undefined;
  if (typeof object.id !== 'string' || object.id.length === 0) return undefined;
  return { owner: object.owner, id: object.id };
}

function messageRef(value: unknown): MessageRef | undefined {
  const object = rec(value);
  if (object === undefined) return undefined;
  if (typeof object.sender !== 'string' || object.sender.length === 0) return undefined;
  if (typeof object.id !== 'string' || object.id.length === 0) return undefined;
  return { sender: object.sender, id: object.id };
}

/**
 * Assembles the Grok Bot node: an ordinary OnlyneNode whose inbound path POSTs each delivered
 * message to the configured webhook. The reply path is a host HTTP surface, not a protocol route.
 */
export async function createGrokBotNode(config: GrokBotConfig, options: GrokBotOptions = {}): Promise<GrokBotNode> {
  const out = leveled(options.log);
  const storage: Storage = options.storage === 'memory' ? new MemoryStorage() : new FileStorage(config.storage_path);
  const owner: NodeId = config.node.id;
  const dispatcher = new WebhookDispatcher({
    storage,
    webhook: config.webhook,
    nodeId: owner,
    ...(options.fetch === undefined ? {} : { fetch: options.fetch }),
    ...(options.now === undefined ? {} : { now: options.now }),
    log: out,
  });
  const pull = new PullInbox({
    storage, nodeId: owner, ...(options.now === undefined ? {} : { now: options.now }),
  });
  const wake = (entry: Entry): void => {
    // Replies this node authors must not re-enter the webhook or pull inbox; a consumer
    // that answers every wake would otherwise loop on itself.
    if (entry.message.sender === owner) return;
    dispatcher.enqueue(entry);
    void pull.enqueue(entry);
  };
  const base_url = `http://${config.server.bind}:${config.server.port}/onlyne/v3`;
  let node: OnlyneNode;
  const authenticate = async (request: Request): Promise<Auth | null> => {
    const actor = request.headers.get('Onlyne-Actor');
    if (actor === null || actor.length === 0) return null;
    if (actor === owner) throw new OnlyneError('forbidden', t('error.claimIdentity'));
    return { actor };
  };
  const plugins: Omit<Plugins, 'storage'> = {
    authenticate,
    maintenanceClock: nodeClock(),
    resolve: async id => {
      const target = id === owner ? { node_id: id, base_url } : await node.discovery.resolve(id);
      return { ...target, headers: { 'Onlyne-Actor': owner } };
    },
    domainCandidates: domain_id => node.discovery.candidates(domain_id),
    executionContexts: async () => (config.contexts ?? ['main']).map(alias => ({
      alias, summary: t('context.summary'), state: 'idle',
    })) as Json,
    ...(options.keys === undefined ? {} : { keys: options.keys }),
    ...(config.trust_seeds === undefined || config.trust_seeds.length === 0
      ? {}
      : { trust_seeds: config.trust_seeds }),
    decide: () => { throw new OnlyneError('decision_unavailable', t('error.decide')); },
    // Do not invent a random first-contact thread. Inbound admissions are bound to
    // grokbot:dm:<agent_id> / grokbot:group:<group_id> before admit (installGrokBotAdmit).
    runtime: {
      notify: event => {
        if (event.event === 'trust') {
          out.log('grok.trust', { node: event.entry.node_id, phase: event.phase, fingerprint: event.entry.fingerprint });
          return;
        }
        if (event.event !== 'inbound') return;
        void enqueueNotify(wake, node.messages, node.auth, event, out);
      },
      supports: async plan => plan.response !== 'await',
      inject: async (entry: Entry) => {
        wake(entry);
        return { context: owner, consumed: true, outcome: 'delivered' as const };
      },
    },
  };
  node = new OnlyneNode({
    node_id: owner,
    base_url,
    ...(options.keys === undefined ? {} : { authentication: ['signature'] }),
    plugins: {
      ...plugins, storage,
      ...(options.transport === undefined ? {} : { transport: options.transport }),
    },
    now: options.now ?? (() => Date.now()),
    id: () => crypto.randomUUID(),
  });
  installGrokBotAdmit(node, owner);

  const replyAuth = (request: Request): boolean => {
    const got = request.headers.get(config.webhook.header);
    if (got === null) return false;
    return headerEqual(got, webhookHeaderValue(config.webhook));
  };

  const handleReply = async (request: Request): Promise<Response> => {
    if (request.method !== 'POST') return new Response('Method Not Allowed', { status: 405 });
    if (!replyAuth(request)) {
      return Response.json({ error: { code: 'unauthenticated', message: t('error.replyAuth') } }, { status: 401 });
    }
    let body: unknown;
    try {
      body = await request.json();
    } catch {
      return Response.json({ error: { code: 'invalid_request', message: t('error.replyBody') } }, { status: 400 });
    }
    const object = rec(body);
    if (object === undefined || typeof object.to !== 'string' || object.to.length === 0
      || typeof object.msg !== 'string') {
      return Response.json({ error: { code: 'invalid_request', message: t('error.replyBody') } }, { status: 400 });
    }
    // The supported continue-conversation body brings back `thread` and `in_reply_to`
    // from the webhook. Omitting `thread` does not invent a grokbot:dm/group id: an
    // `in_reply_to` that this node admitted can recover that landing; both omitted is
    // refused (this node has no default thread).
    const thread = threadRef(object.thread);
    const inReplyTo = messageRef(object.in_reply_to);
    const deliveryKind = typeof object.delivery_kind === 'string' && object.delivery_kind.length > 0
      ? object.delivery_kind : 'reply';
    const message: Message = {
      id: typeof object.id === 'string' && object.id.length > 0 ? object.id : crypto.randomUUID(),
      sender: owner,
      delivery_kind: deliveryKind,
      msg: object.msg,
      ...(thread === undefined && inReplyTo === undefined ? {} : {
        meta: {
          ...(thread === undefined ? {} : { thread }),
          ...(inReplyTo === undefined ? {} : { in_reply_to: inReplyTo, relation: 'reply' as const }),
        },
      }),
    };
    const plan: DeliveryPlan = { response: 'none', execution: 'existing' };
    try {
      const outcome = await node.client.send(object.to, message, plan);
      const status = outcome.admission === 'accepted' ? 201 : outcome.admission === 'refused' ? 422 : 503;
      return Response.json({
        admission: outcome.admission,
        ...(outcome.receipt === undefined ? {} : { receipt: outcome.receipt }),
        ...(outcome.error === undefined ? {} : { error: outcome.error }),
      }, { status });
    } catch (error) {
      if (error instanceof OnlyneError) {
        return Response.json({ error: error.error }, { status: error.status });
      }
      return Response.json({ error: { code: 'unavailable', message: String(error) } }, { status: 503 });
    }
  };

  const handleEvents = async (request: Request): Promise<Response> => {
    if (request.method !== 'GET') return new Response('Method Not Allowed', { status: 405 });
    if (!replyAuth(request)) {
      return Response.json({ error: { code: 'unauthenticated', message: t('error.eventsAuth') } }, { status: 401 });
    }
    const url = new URL(request.url);
    const items = await pull.pull({
      wait_ms: clampWaitMs(url.searchParams.get('wait_ms'), 0),
      limit: clampLimit(url.searchParams.get('limit')),
      lease_ms: clampLeaseMs(url.searchParams.get('lease_ms')),
      signal: request.signal,
    });
    return Response.json({ items });
  };

  const handleAck = async (request: Request): Promise<Response> => {
    if (request.method !== 'POST') return new Response('Method Not Allowed', { status: 405 });
    if (!replyAuth(request)) {
      return Response.json({ error: { code: 'unauthenticated', message: t('error.eventsAuth') } }, { status: 401 });
    }
    let body: unknown;
    try {
      body = await request.json();
    } catch {
      return Response.json({ error: { code: 'invalid_request', message: t('error.ackBody') } }, { status: 400 });
    }
    const object = rec(body);
    if (object === undefined || !Array.isArray(object.ids)) {
      return Response.json({ error: { code: 'invalid_request', message: t('error.ackBody') } }, { status: 400 });
    }
    const ids = object.ids.filter((id): id is string => typeof id === 'string' && id.length > 0);
    return Response.json(await pull.ack(ids));
  };

  const statusOf = async (): Promise<GrokBotStatus> => {
    const snap = await dispatcher.snapshot();
    return {
      node: owner, webhook: snap.webhook, queue: snap.queue, pull: await pull.snapshot(),
      reply_path: config.reply_path, events_path: config.events_path,
    };
  };

  return {
    node, config, storage, dispatcher, pull,
    async fetch(request: Request): Promise<Response> {
      const pathname = new URL(request.url).pathname;
      if (pathname === DEFAULT_STATUS_PATH) return Response.json(await statusOf());
      if (pathname === config.reply_path) return handleReply(request);
      if (pathname === config.events_path) return handleEvents(request);
      if (pathname === config.ack_path) return handleAck(request);
      return node.fetch(request);
    },
    status: statusOf,
    async statusLines(): Promise<string[]> {
      const report = await statusOf();
      return [
        `webhook ${report.webhook.url} header=${report.webhook.header}`,
        `queue owed=${report.queue.owed} failed=${report.queue.failed} delivered=${report.queue.delivered} in_flight=${report.queue.in_flight}`,
        `pull owed=${report.pull.owed} leased=${report.pull.leased} acked=${report.pull.acked}`,
        `reply ${report.reply_path}`,
        `events ${report.events_path}`,
      ];
    },
    maintain: () => dispatcher.retryOwed(),
    async approvePinned(): Promise<void> {
      for (const seed of config.trust_seeds ?? []) {
        try {
          await node.trust.accept(node.auth, seed.node_id, seed.fingerprint);
          out.log('grok.trust.pinned', { node: seed.node_id, fingerprint: seed.fingerprint });
        } catch (error) {
          out.warn('grok.trust.pin_failed', {
            node: seed.node_id, fingerprint: seed.fingerprint, error: String(error),
          });
        }
      }
    },
    async stop(): Promise<void> {
      node.client.stopMaintenance();
    },
  };
}
