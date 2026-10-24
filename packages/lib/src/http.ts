import { OnlyneError, canonical, fail, same } from './core.js';
import type { Context } from './core.js';
import type {
  Auth, DeliveryPlan, DomainMutation, Event, Grant, Json, Member, Message, MessageQuery, Mutation, PageOptions,
  OperationRequest, ReadScope, Resource, ResourceEvent, SendRequest,
} from './types.js';
import type { ThreadService } from './threads.js';
import type { MessageService } from './messages.js';
import type { ResourceService } from './resources.js';
import type { DomainService } from './domains.js';
import type { DiscoveryService } from './discovery.js';
import { validateDomainId } from './domains.js';

const PREFIX = '/onlyne/v3';
const DEFAULT_BYTES = 1_048_576;
const STREAM_TICK_MS = 15_000;
const STREAM_MAX_PAGES = 1_000;

export interface HttpServices {
  ctx: Context;
  threads: ThreadService;
  messages: MessageService;
  resources: ResourceService;
  domains: DomainService;
  discovery?: DiscoveryService;
}

function record(value: unknown, field: string): Record<string, unknown> {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) fail('invalid_request', `${field} must be a JSON object.`);
  return value as Record<string, unknown>;
}

/** Rejects keys this endpoint does not route on, so a typo cannot silently change meaning. */
function shape(value: unknown, field: string, keys: readonly string[]): Record<string, unknown> {
  const object = record(value, field);
  for (const name of Object.keys(object)) {
    if (!keys.includes(name)) fail('invalid_request', `${field}.${name} is not a field this endpoint accepts.`);
  }
  return object;
}

function string(value: unknown, field: string): string {
  if (typeof value !== 'string' || value.length === 0) fail('invalid_request', `${field} must be a non-empty string.`);
  return value;
}

function optionalString(value: unknown, field: string): string | undefined {
  if (value === undefined) return undefined;
  return string(value, field);
}

function list<T>(value: unknown, field: string, item: (entry: unknown, at: string) => T): T[] {
  if (!Array.isArray(value)) fail('invalid_request', `${field} must be an array.`);
  return value.map((entry, index) => item(entry, `${field}[${index}]`));
}

function threadRef(value: unknown, field: string): { owner: string; id: string } {
  const raw = shape(value, field, ['owner', 'id']);
  return { owner: string(raw.owner, `${field}.owner`), id: string(raw.id, `${field}.id`) };
}

function messageRef(value: unknown, field: string): { sender: string; id: string } {
  const raw = shape(value, field, ['sender', 'id']);
  return { sender: string(raw.sender, `${field}.sender`), id: string(raw.id, `${field}.id`) };
}

function resourceRef(value: unknown, field: string): { owner: string; id: string } {
  const raw = shape(value, field, ['owner', 'id']);
  return { owner: string(raw.owner, `${field}.owner`), id: string(raw.id, `${field}.id`) };
}

function deliveryPlan(value: unknown, field: string): DeliveryPlan {
  const raw = shape(value, field, ['response', 'execution', 'context']);
  const response = string(raw.response, `${field}.response`);
  if (response !== 'none' && response !== 'async' && response !== 'await') fail('invalid_request', `${field}.response must be none, async or await.`);
  const execution = string(raw.execution, `${field}.execution`);
  if (execution !== 'existing' && execution !== 'new') fail('invalid_request', `${field}.execution must be existing or new.`);
  const context = optionalString(raw.context, `${field}.context`);
  return { response, execution, ...(context === undefined ? {} : { context }) };
}

function readScope(value: unknown, field: string): ReadScope {
  const raw = shape(value, field, ['peers', 'since_seq', 'related_to']);
  const since = optionalString(raw.since_seq, `${field}.since_seq`);
  if (since !== undefined && !/^(0|[1-9][0-9]*)$/.test(since)) fail('invalid_request', `${field}.since_seq must be a decimal sequence string.`);
  return {
    ...(raw.peers === undefined ? {} : { peers: list(raw.peers, `${field}.peers`, (entry, at) => string(entry, at)) }),
    ...(since === undefined ? {} : { since_seq: since }),
    ...(raw.related_to === undefined ? {} : { related_to: messageRef(raw.related_to, `${field}.related_to`) }),
  };
}

function messageMeta(value: unknown, field: string): NonNullable<Message['meta']> {
  const raw = shape(value, field, ['thread', 'in_reply_to', 'relation', 'mentions', 'parts', 'data']);
  const relation = optionalString(raw.relation, `${field}.relation`);
  if (relation !== undefined && relation !== 'reply' && relation !== 'followup') fail('invalid_request', `${field}.relation must be reply or followup.`);
  return {
    ...(raw.thread === undefined ? {} : { thread: threadRef(raw.thread, `${field}.thread`) }),
    ...(raw.in_reply_to === undefined ? {} : { in_reply_to: messageRef(raw.in_reply_to, `${field}.in_reply_to`) }),
    ...(relation === undefined ? {} : { relation }),
    ...(raw.mentions === undefined ? {} : { mentions: list(raw.mentions, `${field}.mentions`, (entry, at) => string(entry, at)) }),
    ...(raw.parts === undefined ? {} : { parts: list(raw.parts, `${field}.parts`, entry => entry as Json) }),
    ...(raw.data === undefined ? {} : { data: record(raw.data, `${field}.data`) as NonNullable<Message['meta']>['data'] }),
  };
}

function message(value: unknown, field: string): Message {
  const raw = shape(value, field, ['id', 'sender', 'delivery_kind', 'msg', 'meta']);
  return {
    id: string(raw.id, `${field}.id`),
    sender: string(raw.sender, `${field}.sender`),
    delivery_kind: string(raw.delivery_kind, `${field}.delivery_kind`),
    msg: string(raw.msg, `${field}.msg`),
    ...(raw.meta === undefined ? {} : { meta: messageMeta(raw.meta, `${field}.meta`) }),
  };
}

function memberPatch(value: Record<string, unknown>, field: string): Partial<Member> {
  const state = optionalString(value.state, `${field}.state`);
  if (state !== undefined && state !== 'requested' && state !== 'active' && state !== 'revoked' && state !== 'denied') {
    fail('invalid_request', `${field}.state must be requested, active, revoked or denied.`);
  }
  const access = value.access === undefined ? undefined : list(value.access, `${field}.access`, (entry, at) => {
    const name = string(entry, at);
    if (name !== 'read' && name !== 'post') fail('invalid_request', `${at} must be read or post.`);
    return name;
  });
  return {
    ...(value.peer === undefined ? {} : { peer: string(value.peer, `${field}.peer`) }),
    ...(state === undefined ? {} : { state: state as Member['state'] }),
    ...(access === undefined ? {} : { access: access as Member['access'] }),
    ...(value.read_scope === undefined ? {} : { read_scope: readScope(value.read_scope, `${field}.read_scope`) }),
  };
}

function pageOptions(params: URLSearchParams): PageOptions {
  const cursor = params.get('cursor');
  const size = params.get('page_size');
  if (size !== null && !/^([1-9][0-9]?|100)$/.test(size)) fail('invalid_request', 'page_size must be from 1 to 100.');
  return { ...(cursor === null ? {} : { cursor }), ...(size === null ? {} : { page_size: Number(size) }) };
}

function messageQuery(params: URLSearchParams): MessageQuery {
  const attention = params.get('attention');
  if (attention !== null && attention !== 'inbox' && attention !== 'thread') fail('invalid_request', 'attention must be inbox or thread.');
  const since = params.get('since_seq');
  if (since !== null && !/^(0|[1-9][0-9]*)$/.test(since)) fail('invalid_request', 'since_seq must be a decimal sequence string.');
  const related = params.get('related_to');
  let related_to: MessageQuery['related_to'];
  if (related !== null) {
    let parsed: unknown;
    try { parsed = JSON.parse(related); } catch { fail('invalid_request', 'related_to must encode a MessageRef as JSON.'); }
    try { related_to = messageRef(parsed, 'related_to'); } catch (error) {
      if (error instanceof OnlyneError) fail('invalid_request', 'related_to must encode a {sender, id} MessageRef.');
      throw error;
    }
  }
  return {
    ...pageOptions(params),
    ...(attention === null ? {} : { attention }),
    ...(since === null ? {} : { since_seq: since }),
    ...(params.get('peer') === null ? {} : { peer: string(params.get('peer'), 'peer') }),
    ...(related_to === undefined ? {} : { related_to }),
  };
}

async function json(request: Request, limit: number): Promise<unknown> {
  const declared = request.headers.get('content-length');
  if (declared !== null && /^[0-9]+$/.test(declared) && Number(declared) > limit) {
    fail('payload_too_large', `The request body exceeds ${limit} bytes.`);
  }
  const type = (request.headers.get('content-type') ?? '').split(';')[0]!.trim().toLowerCase();
  if (type !== 'application/json') fail('unsupported_media_type', 'Content-Type must be application/json.');
  if (request.body === null) fail('invalid_request', 'A JSON body is required.');
  const chunks: Uint8Array[] = [];
  let total = 0;
  const reader = request.body.getReader();
  while (true) {
    const { done, value } = await reader.read();
    if (done === true) break;
    total += value.byteLength;
    if (total > limit) fail('payload_too_large', `The request body exceeds ${limit} bytes.`);
    chunks.push(value);
  }
  if (total === 0) fail('invalid_request', 'A JSON body is required.');
  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
  try {
    return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes));
  } catch {
    fail('invalid_request', 'The request body is not valid UTF-8 JSON.');
  }
}

function mutation(request: Request, raw: Record<string, unknown>): Mutation {
  const header = request.headers.get('idempotency-key');
  const field = optionalString(raw.idempotency_key, 'idempotency_key');
  if (header !== null && field !== undefined && header !== field) {
    fail('invalid_request', 'Idempotency-Key and idempotency_key disagree.');
  }
  const idempotency_key = header ?? field;
  if (idempotency_key === undefined) fail('invalid_request', 'An Idempotency-Key header is required.');
  const headerDeadline = request.headers.get('x-onlyne-retry-until');
  const fieldDeadline = optionalString(raw.retry_until, 'retry_until');
  if (headerDeadline !== null && fieldDeadline !== undefined && headerDeadline !== fieldDeadline) {
    fail('invalid_request', 'X-Onlyne-Retry-Until and retry_until disagree.');
  }
  return { idempotency_key, retry_until: string(headerDeadline ?? fieldDeadline, 'retry_until') };
}

/** DELETE carries no body, so its mutation comes from the headers and is checked the same way. */
function headerMutation(request: Request): Mutation {
  return mutation(request, {});
}

function sendRequest(request: Request, value: unknown, at: { owner: string; id: string } | undefined): SendRequest {
  const raw = shape(value, 'body', ['idempotency_key', 'retry_until', 'message', 'target_endpoint', 'plan']);
  const content = message(raw.message, 'message');
  if (at !== undefined) {
    const declared = content.meta?.thread;
    if (declared !== undefined && (declared.owner !== at.owner || declared.id !== at.id)) {
      fail('invalid_request', 'The message thread must match the request path.');
    }
    content.meta = { ...content.meta, thread: at };
  }
  return {
    ...mutation(request, raw),
    message: content,
    target_endpoint: string(raw.target_endpoint, 'target_endpoint'),
    plan: deliveryPlan(raw.plan, 'plan'),
  };
}

function grant(value: unknown): Grant {
  const raw = shape(value, 'grant', ['id', 'recipient', 'thread_ref', 'revision', 'acked']);
  if (typeof raw.acked !== 'boolean') fail('invalid_request', 'grant.acked must be a boolean.');
  return {
    id: string(raw.id, 'grant.id'),
    recipient: string(raw.recipient, 'grant.recipient'),
    thread_ref: threadRef(raw.thread_ref, 'grant.thread_ref'),
    revision: string(raw.revision, 'grant.revision'),
    acked: raw.acked,
  };
}

function resourceRecord(value: unknown): Resource {
  const raw = shape(value, 'resource', [
    'resource_ref', 'kind', 'operations', 'assignee', 'revision', 'readers', 'controllers',
    'event_recipients', 'result_recipients', 'event_seq', 'retention_ms', 'data',
  ]);
  const kind = string(raw.kind, 'resource.kind');
  if (kind !== 'im_conversation' && kind !== 'compute_job' && kind !== 'dataset' && kind !== 'mcp') {
    fail('invalid_request', 'resource.kind must be im_conversation, compute_job, dataset or mcp.');
  }
  const nodes = (name: string) => list(raw[name] ?? [], `resource.${name}`, (entry, at) => string(entry, at));
  const eventSeq = optionalString(raw.event_seq, 'resource.event_seq');
  if (eventSeq !== undefined && !/^(0|[1-9][0-9]*)$/.test(eventSeq)) fail('invalid_request', 'resource.event_seq must be a decimal sequence string.');
  if (raw.retention_ms !== undefined && (typeof raw.retention_ms !== 'number' || !Number.isInteger(raw.retention_ms) || raw.retention_ms < 0)) {
    fail('invalid_request', 'resource.retention_ms must be a non-negative integer.');
  }
  return {
    resource_ref: resourceRef(raw.resource_ref, 'resource.resource_ref'),
    kind,
    operations: list(raw.operations ?? [], 'resource.operations', (entry, at) => string(entry, at)),
    ...(raw.assignee === undefined ? {} : { assignee: string(raw.assignee, 'resource.assignee') }),
    revision: string(raw.revision, 'resource.revision'),
    readers: nodes('readers'),
    controllers: nodes('controllers'),
    event_recipients: nodes('event_recipients'),
    result_recipients: nodes('result_recipients'),
    event_seq: eventSeq ?? '0',
    retention_ms: (raw.retention_ms as number | undefined) ?? 0,
    ...(raw.data === undefined ? {} : { data: raw.data as Resource['data'] }),
  };
}

function operationRequest(request: Request, value: unknown): OperationRequest {
  const raw = shape(value, 'body', ['idempotency_key', 'retry_until', 'operation_id', 'operation', 'expected_revision', 'data']);
  return {
    ...mutation(request, raw),
    operation_id: string(raw.operation_id, 'operation_id'),
    operation: string(raw.operation, 'operation'),
    ...(raw.expected_revision === undefined ? {} : { expected_revision: string(raw.expected_revision, 'expected_revision') }),
    ...(raw.data === undefined ? {} : { data: raw.data as Json }),
  };
}

function eventBody(value: unknown): { source_id: string; data: Json; expires_at?: string } {
  const raw = shape(value, 'body', ['source_id', 'data', 'expires_at']);
  if (!('data' in raw)) fail('invalid_request', 'data is required.');
  return {
    source_id: string(raw.source_id, 'source_id'),
    data: raw.data as Json,
    ...(raw.expires_at === undefined ? {} : { expires_at: string(raw.expires_at, 'expires_at') }),
  };
}

/**
 * The public description: identity, address, current region declaration and configured
 * authentication schemes. It carries no credential subject, message or resource.
 */
async function description(ctx: Context, domains: DomainService, discovered: boolean): Promise<Json> {
  const active = (await domains.list({ actor: ctx.node }, { page_size: 100 })).items.find(membership => membership.state === 'active');
  return {
    protocol: 'onlyne/3',
    node_id: ctx.node,
    instance_id: ctx.instanceId,
    base_url: ctx.baseUrl,
    features: [
      'threads', 'messages', 'events', 'resources', 'domains',
      ...(discovered ? ['discovery'] : []),
      ...(ctx.options.plugins.executionContexts === undefined ? [] : ['contexts']),
    ],
    // The binding names only schemes its host configured; a credential's subject is not a scheme.
    authentication: ctx.options.authentication ?? [],
    domain: active?.domain_id ?? null,
    membership: active === undefined
      ? null
      : { domain_id: active.domain_id, node_id: active.node_id, state: active.state, revision: active.revision },
    limits: { request_bytes: ctx.options.request_bytes ?? DEFAULT_BYTES },
    idempotency_max_seconds: Math.floor((ctx.options.retry_max_ms ?? 86_400_000) / 1000),
  };
}

/** A resume cursor this binding issues: a decimal event position bound to one stream scope. */
function eventCursor(value: unknown): { scope: unknown; after: string } | undefined {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) return undefined;
  if (!('after' in value) || !('scope' in value)) return undefined;
  const { after, scope } = value;
  if (typeof after !== 'string' || !/^(0|[1-9][0-9]*)$/.test(after)) return undefined;
  return { scope, after };
}

function respond(value: unknown, status = 200): Response {
  return new Response(JSON.stringify(value ?? null), {
    status,
    headers: { 'content-type': 'application/json', 'cache-control': 'no-store' },
  });
}

function failure(error: unknown): Response {
  const onlyne = error instanceof OnlyneError ? error : new OnlyneError('internal', 'The node failed to complete the request.');
  return respond({ error: onlyne.error }, onlyne.status);
}

function expect(request: Request, allowed: string): void {
  if (request.method !== allowed) fail('not_found', `This endpoint does not accept ${request.method}.`);
}

/** Maps `/onlyne/v3` HTTP requests onto the services. The caller owns the server lifecycle. */
export function createHttpHandler({ ctx, threads, messages, resources, domains, discovery }: HttpServices): (request: Request) => Promise<Response> {
  const limit = ctx.options.request_bytes ?? DEFAULT_BYTES;

  async function authenticate(request: Request): Promise<Auth> {
    const auth = await ctx.options.plugins.authenticate(request);
    if (auth === null || typeof auth.actor !== 'string' || auth.actor.length === 0) fail('unauthenticated', 'Authentication is required.');
    return auth;
  }

  /**
   * Polls the event page on a timer and writes SSE frames. Every frame carries its own `id:`: a
   * cursor bound to this node, actor, ThreadRef and query scope, positioned after the event the
   * frame delivered. A frame that reused one page cursor would replay the whole page on resume.
   * The resume value is validated against that same scope before the response exists, the first
   * page is fetched up front so a lost credential or expired cursor answers with an HTTP error
   * instead of a 200 stream, and every later page re-checks the credential.
   */
  async function stream(request: Request, auth: Auth, ref: { owner: string; id: string }): Promise<Response> {
    const encoder = new TextEncoder();
    const scope = { ref, actor: auth.actor, kind: 'events' as const };
    const resume = request.headers.get('last-event-id');
    let cursor: string | undefined;
    if (resume !== null && resume.length > 0) {
      let decoded: unknown;
      try {
        decoded = JSON.parse(decodeURIComponent(resume));
      } catch {
        fail('invalid_request', 'Last-Event-ID is not a cursor this binding issues.');
      }
      const parsed = eventCursor(decoded);
      if (parsed === undefined || !same(parsed.scope, scope)) {
        fail('invalid_request', 'Last-Event-ID belongs to another node, actor or thread.');
      }
      cursor = resume;
    }

    let tick: ReturnType<typeof setInterval> | undefined;
    let running = false;
    let done = false;
    let detach = () => {};

    const write = (controller: ReadableStreamDefaultController<Uint8Array>, text: string) => {
      if (done) return;
      try {
        controller.enqueue(encoder.encode(text));
      } catch {
        // The consumer closed the stream; release the tick and the abort listener with it.
        done = true;
        clearInterval(tick);
        detach();
      }
    };
    const stop = (controller: ReadableStreamDefaultController<Uint8Array>) => {
      if (done) return;
      done = true;
      clearInterval(tick);
      detach();
      try { controller.close(); } catch { /* the consumer already closed it */ }
    };
    const emit = (controller: ReadableStreamDefaultController<Uint8Array>, event: Event) => {
      cursor = encodeURIComponent(canonical({ scope, after: event.id }));
      write(controller, `id: ${cursor}\nevent: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`);
    };
    const drain = async (controller: ReadableStreamDefaultController<Uint8Array>) => {
      for (let pages = 0; pages < STREAM_MAX_PAGES && !done; pages++) {
        const page = await messages.events(auth, ref, cursor === undefined ? {} : { cursor });
        if (page.items.length === 0) {
          write(controller, ': idle\n\n');
          return;
        }
        for (const event of page.items) emit(controller, event);
        if (page.next_cursor === null) return;
      }
    };
    const poll = async (controller: ReadableStreamDefaultController<Uint8Array>) => {
      if (running || done) return;
      running = true;
      try {
        const current = await ctx.options.plugins.authenticate(request);
        if (current === null || current.actor !== auth.actor) {
          write(controller, `event: error\ndata: ${JSON.stringify({ error: new OnlyneError('forbidden', 'The credential is no longer valid for this stream.').error })}\n\n`);
          stop(controller);
          return;
        }
        await drain(controller);
      } catch (error) {
        const onlyne = error instanceof OnlyneError ? error : new OnlyneError('internal', 'The event stream failed.');
        write(controller, `event: error\ndata: ${JSON.stringify({ error: onlyne.error })}\n\n`);
        stop(controller);
      } finally {
        running = false;
      }
    };

    // The first page settles authorization and cursor validity while a status code can still say so.
    const first = await messages.events(auth, ref, cursor === undefined ? {} : { cursor });
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        for (const event of first.items) emit(controller, event);
        if (first.items.length === 0) write(controller, ': idle\n\n');
        if (request.signal.aborted) {
          stop(controller);
          return;
        }
        const onAbort = () => stop(controller);
        detach = () => request.signal.removeEventListener('abort', onAbort);
        request.signal.addEventListener('abort', onAbort, { once: true });
        tick = setInterval(() => { void poll(controller); }, STREAM_TICK_MS);
        if (first.next_cursor !== null) void poll(controller);
      },
      cancel() {
        done = true;
        clearInterval(tick);
        detach();
      },
    });
    return new Response(body, {
      status: 200,
      headers: { 'content-type': 'text/event-stream', 'cache-control': 'no-store', connection: 'keep-alive' },
    });
  }

  return async function handle(request: Request): Promise<Response> {
    try {
      const url = new URL(request.url);
      if (!url.pathname.startsWith(`${PREFIX}/`)) fail('not_found', 'Unknown route.');
      const path = url.pathname.slice(PREFIX.length).split('/').filter(segment => segment.length > 0).map(segment => {
        try {
          return decodeURIComponent(segment);
        } catch {
          return fail('invalid_request', 'The request path is not valid percent-encoding.');
        }
      });
      const params = url.searchParams;

      if (path.length === 1 && path[0] === 'description') {
        expect(request, 'GET');
        return respond(await description(ctx, domains, discovery !== undefined));
      }

      if (path.length === 1 && path[0] === 'discovery') {
        expect(request, 'GET');
        if (discovery === undefined) fail('not_found', 'Unknown route.');
        return respond(await discovery.list(pageOptions(params)));
      }

      const auth = await authenticate(request);

      if (path.length === 1 && path[0] === 'contexts') {
        expect(request, 'GET');
        const contexts = ctx.options.plugins.executionContexts;
        if (contexts === undefined) fail('unsupported_delivery', 'The host provides no execution context directory.');
        return respond(await contexts());
      }

      if (path.length === 1 && path[0] === 'messages') {
        expect(request, 'POST');
        return respond(await messages.admit(auth, sendRequest(request, await json(request, limit), undefined)), 201);
      }

      if (path.length === 1 && path[0] === 'threads') {
        if (request.method === 'GET') return respond(await threads.list(auth, pageOptions(params)));
        if (request.method === 'POST') {
          const raw = shape(await json(request, limit), 'body', ['idempotency_key', 'retry_until', 'title', 'id', 'default']);
          if (raw.default !== undefined && typeof raw.default !== 'boolean') fail('invalid_request', 'default must be a boolean.');
          const id = optionalString(raw.id, 'id');
          const created = await threads.create(auth, {
            ...mutation(request, raw),
            title: string(raw.title, 'title'),
            ...(id === undefined ? {} : { id }),
            ...(raw.default === undefined ? {} : { default: raw.default }),
          });
          return respond(created, 201);
        }
        fail('not_found', 'Unknown route.');
      }

      if (path.length === 2 && path[0] === 'resources' && path[1] === 'invitations') {
        expect(request, 'GET');
        return respond(await resources.invitations(auth, pageOptions(params)));
      }

      if (path.length === 1 && path[0] === 'resources') {
        if (request.method === 'GET') return respond(await resources.list(auth, pageOptions(params)));
        if (request.method === 'POST') {
          const raw = shape(await json(request, limit), 'body', ['idempotency_key', 'retry_until', 'resource']);
          mutation(request, raw);
          return respond(await resources.register(auth, resourceRecord(raw.resource)), 201);
        }
        fail('not_found', 'Unknown route.');
      }

      if (path.length === 1 && path[0] === 'grants') {
        if (request.method === 'GET') return respond(await threads.grants(auth, pageOptions(params)));
        if (request.method === 'POST') {
          const raw = shape(await json(request, limit), 'body', ['grant']);
          return respond(await threads.acceptGrant(auth, grant(raw.grant)), 201);
        }
        fail('not_found', 'Unknown route.');
      }

      if (path.length === 3 && path[0] === 'grants' && path[2] === 'ack') {
        expect(request, 'POST');
        const raw = shape(await json(request, limit), 'body', ['idempotency_key', 'retry_until']);
        return respond(await threads.ackGrant(auth, path[1]!, mutation(request, raw)));
      }

      if (path.length === 1 && path[0] === 'domains') {
        if (request.method === 'GET') return respond(await domains.list(auth, pageOptions(params)));
        if (request.method === 'POST') {
          const raw = shape(await json(request, limit), 'body', ['idempotency_key', 'retry_until']);
          // Membership belongs to the node that holds it; a peer's credential never mutates it.
          ctx.owner(auth.actor);
          return respond(await domains.create(auth, mutation(request, raw)), 201);
        }
        fail('not_found', 'Unknown route.');
      }

      if (path.length === 2 && path[0] === 'domains') {
        expect(request, 'GET');
        return respond(await domains.describe(auth, validateDomainId(path[1])));
      }

      if (path.length === 3 && path[0] === 'domains') {
        const id = validateDomainId(path[1]);
        if (path[2] === 'members') {
          expect(request, 'GET');
          return respond(await domains.discover(auth, id, request.signal));
        }
        if (path[2] === 'join' || path[2] === 'leave') {
          expect(request, 'POST');
          const raw = shape(await json(request, limit), 'body', ['idempotency_key', 'retry_until', 'expected_revision']);
          const expected = optionalString(raw.expected_revision, 'expected_revision');
          ctx.owner(auth.actor);
          const change: DomainMutation = {
            ...mutation(request, raw),
            ...(expected === undefined ? {} : { expected_revision: expected }),
          };
          return respond(await (path[2] === 'join' ? domains.join(auth, id, change) : domains.leave(auth, id, change)));
        }
        fail('not_found', 'Unknown route.');
      }

      if (path.length >= 3 && path[0] === 'threads') {
        const ref = { owner: path[1]!, id: path[2]! };
        const tail = path[3];

        if (tail === undefined) {
          expect(request, 'GET');
          return respond(await threads.describe(auth, ref));
        }

        if (tail === 'close') {
          if (path.length !== 4) fail('not_found', 'Unknown route.');
          expect(request, 'POST');
          const raw = shape(await json(request, limit), 'body', ['idempotency_key', 'retry_until']);
          return respond(await threads.close(auth, ref, mutation(request, raw)));
        }

        if (tail === 'memberships') {
          if (path.length === 4) {
            if (request.method === 'GET') return respond(await threads.members(auth, ref, pageOptions(params)));
            if (request.method === 'POST') {
              const raw = shape(await json(request, limit), 'body', ['idempotency_key', 'retry_until', 'peer', 'state', 'access', 'read_scope']);
              return respond(await threads.member(auth, ref, string(raw.peer, 'peer'), memberPatch(raw, 'body'), mutation(request, raw)), 201);
            }
            fail('not_found', 'Unknown route.');
          }
          if (path.length === 5) {
            expect(request, 'DELETE');
            const peer = path[4] === 'self' ? auth.actor : path[4]!;
            return respond(await threads.member(auth, ref, peer, { state: 'revoked' }, headerMutation(request)));
          }
          fail('not_found', 'Unknown route.');
        }

        if (tail === 'messages') {
          if (path.length !== 4) fail('not_found', 'Unknown route.');
          if (request.method === 'GET') return respond(await messages.list(auth, ref, messageQuery(params)));
          if (request.method === 'POST') return respond(await messages.admit(auth, sendRequest(request, await json(request, limit), ref)), 201);
          fail('not_found', 'Unknown route.');
        }

        if (tail === 'snapshot') {
          if (path.length !== 4) fail('not_found', 'Unknown route.');
          expect(request, 'GET');
          return respond(await messages.snapshot(auth, ref));
        }

        if (tail === 'attention') {
          if (path.length === 4) {
            expect(request, 'GET');
            return respond(await messages.attention(auth, ref, pageOptions(params)));
          }
          if (path.length === 6 && path[5] === 'ack') {
            expect(request, 'POST');
            const raw = shape(await json(request, limit), 'body', ['idempotency_key', 'retry_until']);
            return respond(await messages.ack(auth, ref, path[4]!, mutation(request, raw)));
          }
          fail('not_found', 'Unknown route.');
        }

        if (tail === 'events') {
          if (path.length !== 4) fail('not_found', 'Unknown route.');
          expect(request, 'GET');
          // Awaited so an unauthorized or expired resume answers with a status, not a dead stream.
          return await stream(request, auth, ref);
        }

        fail('not_found', 'Unknown route.');
      }

      if (path.length >= 3 && path[0] === 'resources') {
        const ref = { owner: path[1]!, id: path[2]! };
        const tail = path[3];

        if (tail === undefined) {
          expect(request, 'GET');
          return respond(await resources.describe(auth, ref));
        }

        if (tail === 'operations') {
          if (path.length === 4) {
            if (request.method === 'GET') return respond(await resources.operations(auth, ref, pageOptions(params)));
            if (request.method === 'POST') {
              return respond(await resources.operate(auth, ref, operationRequest(request, await json(request, limit)), request.signal), 201);
            }
            fail('not_found', 'Unknown route.');
          }
          if (path.length === 5) {
            expect(request, 'GET');
            return respond(await resources.operation(auth, ref, path[4]!));
          }
          fail('not_found', 'Unknown route.');
        }

        if (tail === 'events') {
          if (path.length === 4) {
            if (request.method === 'GET') return respond(await resources.events(auth, ref, pageOptions(params)));
            if (request.method === 'POST') {
              const event: ResourceEvent = await resources.receive(auth, ref, eventBody(await json(request, limit)));
              return respond(event, 201);
            }
            fail('not_found', 'Unknown route.');
          }
          if (path.length === 5) {
            expect(request, 'GET');
            return respond(await resources.event(auth, ref, path[4]!));
          }
          if (path.length === 6 && path[5] === 'ack') {
            expect(request, 'POST');
            const raw = shape(await json(request, limit), 'body', ['idempotency_key', 'retry_until']);
            return respond(await resources.ack(auth, ref, path[4]!, mutation(request, raw)));
          }
          fail('not_found', 'Unknown route.');
        }

        if (tail === 'handovers') {
          if (path.length !== 6 || path[5] !== 'decision') fail('not_found', 'Unknown route.');
          expect(request, 'POST');
          const raw = shape(await json(request, limit), 'body', ['idempotency_key', 'retry_until', 'accepted']);
          if (typeof raw.accepted !== 'boolean') fail('invalid_request', 'accepted must be a boolean.');
          return respond(await resources.decideHandover(auth, ref, path[4]!, raw.accepted, mutation(request, raw)));
        }

        fail('not_found', 'Unknown route.');
      }

      fail('not_found', 'Unknown route.');
    } catch (error) {
      return failure(error);
    }
  };
}
