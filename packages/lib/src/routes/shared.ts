import { OnlyneError, fail } from '../core.ts';
import type { Context } from '../core.ts';
import type {
  Auth, DeliveryPlan, Event, Grant, HandoverProbe, Json, LogQuery, Member, Message, MessageQuery, Mutation,
  OperationRequest, PageOptions, ReadScope, Resource, ResourceEvent, SendRequest,
} from '../types.ts';
import type { ThreadService } from '../threads.ts';
import type { MessageService } from '../messages.ts';
import type { ResourceService } from '../resources.ts';
import type { DomainService } from '../domains.ts';
import type { DiscoveryService } from '../discovery.ts';
import type { KeyService, TrustService } from '../keys.ts';

export const DEFAULT_BYTES = 1_048_576;

/** Everything a route module reads from the binding; `createHttpHandler` owns building it. */
export interface RouteRequest {
  request: Request;
  path: string[];
  params: URLSearchParams;
  ctx: Context;
  threads: ThreadService;
  messages: MessageService;
  resources: ResourceService;
  domains: DomainService;
  discovery?: DiscoveryService;
  keys?: KeyService;
  trust?: TrustService;
  auth: Auth;
  rawBody: string;
  limit: number;
}

/** The same binding facts for the routes that answer before any credential is checked. */
export type PublicRouteRequest = Omit<RouteRequest, 'auth' | 'rawBody'>;

export function record(value: unknown, field: string): Record<string, unknown> {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) fail('invalid_request', `${field} must be a JSON object.`);
  return value as Record<string, unknown>;
}

/** Rejects keys this endpoint does not route on, so a typo cannot silently change meaning. */
export function shape(value: unknown, field: string, keys: readonly string[]): Record<string, unknown> {
  const object = record(value, field);
  for (const name of Object.keys(object)) {
    if (!keys.includes(name)) fail('invalid_request', `${field}.${name} is not a field this endpoint accepts.`);
  }
  return object;
}

export function string(value: unknown, field: string): string {
  if (typeof value !== 'string' || value.length === 0) fail('invalid_request', `${field} must be a non-empty string.`);
  return value;
}

export function optionalString(value: unknown, field: string): string | undefined {
  if (value === undefined) return undefined;
  return string(value, field);
}

export function list<T>(value: unknown, field: string, item: (entry: unknown, at: string) => T): T[] {
  if (!Array.isArray(value)) fail('invalid_request', `${field} must be an array.`);
  return value.map((entry, index) => item(entry, `${field}[${index}]`));
}

export function threadRef(value: unknown, field: string): { owner: string; id: string } {
  const raw = shape(value, field, ['owner', 'id']);
  return { owner: string(raw.owner, `${field}.owner`), id: string(raw.id, `${field}.id`) };
}

export function messageRef(value: unknown, field: string): { sender: string; id: string } {
  const raw = shape(value, field, ['sender', 'id']);
  return { sender: string(raw.sender, `${field}.sender`), id: string(raw.id, `${field}.id`) };
}

export function resourceRef(value: unknown, field: string): { owner: string; id: string } {
  const raw = shape(value, field, ['owner', 'id']);
  return { owner: string(raw.owner, `${field}.owner`), id: string(raw.id, `${field}.id`) };
}

export function deliveryPlan(value: unknown, field: string): DeliveryPlan {
  const raw = shape(value, field, ['response', 'execution', 'context']);
  const response = string(raw.response, `${field}.response`);
  if (response !== 'none' && response !== 'async' && response !== 'await') fail('invalid_request', `${field}.response must be none, async or await.`);
  const execution = string(raw.execution, `${field}.execution`);
  if (execution !== 'existing' && execution !== 'new') fail('invalid_request', `${field}.execution must be existing or new.`);
  const context = optionalString(raw.context, `${field}.context`);
  return { response, execution, ...(context === undefined ? {} : { context }) };
}

export function readScope(value: unknown, field: string): ReadScope {
  const raw = shape(value, field, ['peers', 'since_seq', 'related_to']);
  const since = optionalString(raw.since_seq, `${field}.since_seq`);
  if (since !== undefined && !/^(0|[1-9][0-9]*)$/.test(since)) fail('invalid_request', `${field}.since_seq must be a decimal sequence string.`);
  return {
    ...(raw.peers === undefined ? {} : { peers: list(raw.peers, `${field}.peers`, (entry, at) => string(entry, at)) }),
    ...(since === undefined ? {} : { since_seq: since }),
    ...(raw.related_to === undefined ? {} : { related_to: messageRef(raw.related_to, `${field}.related_to`) }),
  };
}

export function messageMeta(value: unknown, field: string): NonNullable<Message['meta']> {
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

export function message(value: unknown, field: string): Message {
  const raw = shape(value, field, ['id', 'sender', 'delivery_kind', 'msg', 'meta']);
  return {
    id: string(raw.id, `${field}.id`),
    sender: string(raw.sender, `${field}.sender`),
    delivery_kind: string(raw.delivery_kind, `${field}.delivery_kind`),
    msg: string(raw.msg, `${field}.msg`),
    ...(raw.meta === undefined ? {} : { meta: messageMeta(raw.meta, `${field}.meta`) }),
  };
}

export function memberPatch(value: Record<string, unknown>, field: string): Partial<Member> {
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

export function pageOptions(params: URLSearchParams): PageOptions {
  const cursor = params.get('cursor');
  const size = params.get('page_size');
  if (size !== null && !/^([1-9][0-9]?|100)$/.test(size)) fail('invalid_request', 'page_size must be from 1 to 100.');
  return { ...(cursor === null ? {} : { cursor }), ...(size === null ? {} : { page_size: Number(size) }) };
}

/** RFC 0006 §6: `since` and `limit` are the log's own query names; `cursor` pages past the last page. */
export function logQuery(params: URLSearchParams): LogQuery {
  const cursor = params.get('cursor');
  const since = params.get('since');
  const limit = params.get('limit');
  if (since !== null && !/^(0|[1-9][0-9]*)$/.test(since)) fail('invalid_request', 'since must be a decimal sequence string.');
  if (limit !== null && !/^([1-9][0-9]?|100)$/.test(limit)) fail('invalid_request', 'limit must be from 1 to 100.');
  return {
    ...(cursor === null ? {} : { cursor }),
    ...(since === null ? {} : { since }),
    ...(limit === null ? {} : { page_size: Number(limit) }),
  };
}

export function messageQuery(params: URLSearchParams): MessageQuery {
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

export async function json(request: Request, limit: number, rawBody?: string): Promise<unknown> {
  const declared = request.headers.get('content-length');
  if (declared !== null && /^[0-9]+$/.test(declared) && Number(declared) > limit) {
    fail('payload_too_large', `The request body exceeds ${limit} bytes.`);
  }
  const type = (request.headers.get('content-type') ?? '').split(';')[0]!.trim().toLowerCase();
  if (type !== 'application/json') fail('unsupported_media_type', 'Content-Type must be application/json.');
  if (rawBody !== undefined) {
    if (rawBody.length === 0) fail('invalid_request', 'A JSON body is required.');
    try {
      return JSON.parse(rawBody);
    } catch {
      fail('invalid_request', 'The request body is not valid UTF-8 JSON.');
    }
  }
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

export function mutation(request: Request, raw: Record<string, unknown>): Mutation {
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
export function headerMutation(request: Request): Mutation {
  return mutation(request, {});
}

export function sendRequest(request: Request, value: unknown, at: { owner: string; id: string } | undefined): SendRequest {
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

export function grant(value: unknown): Grant {
  const raw = shape(value, 'grant', ['id', 'recipient', 'thread_ref', 'revision', 'acked', 'previous_thread_ref']);
  if (typeof raw.acked !== 'boolean') fail('invalid_request', 'grant.acked must be a boolean.');
  const previous = raw.previous_thread_ref === undefined
    ? undefined
    : threadRef(raw.previous_thread_ref, 'grant.previous_thread_ref');
  return {
    id: string(raw.id, 'grant.id'),
    recipient: string(raw.recipient, 'grant.recipient'),
    thread_ref: threadRef(raw.thread_ref, 'grant.thread_ref'),
    revision: string(raw.revision, 'grant.revision'),
    acked: raw.acked,
    ...(previous === undefined ? {} : { previous_thread_ref: previous }),
  };
}

export function resourceRecord(value: unknown): Resource {
  const raw = shape(value, 'resource', [
    'resource_ref', 'kind', 'operations', 'binding', 'log_readers', 'revision', 'event_seq', 'retention_ms', 'data',
  ]);
  const binding = shape(raw.binding, 'resource.binding', ['handler', 'cursor']);
  const cursor = string(binding.cursor, 'resource.binding.cursor');
  if (!/^(0|[1-9][0-9]*)$/.test(cursor)) fail('invalid_request', 'resource.binding.cursor must be a decimal sequence string.');
  const nodes = (name: string) => list(raw[name] ?? [], `resource.${name}`, (entry, at) => string(entry, at));
  const eventSeq = optionalString(raw.event_seq, 'resource.event_seq');
  if (eventSeq !== undefined && !/^(0|[1-9][0-9]*)$/.test(eventSeq)) fail('invalid_request', 'resource.event_seq must be a decimal sequence string.');
  if (raw.retention_ms !== undefined && (typeof raw.retention_ms !== 'number' || !Number.isInteger(raw.retention_ms) || raw.retention_ms < 0)) {
    fail('invalid_request', 'resource.retention_ms must be a non-negative integer.');
  }
  return {
    resource_ref: resourceRef(raw.resource_ref, 'resource.resource_ref'),
    kind: string(raw.kind, 'resource.kind'),
    operations: list(raw.operations ?? [], 'resource.operations', (entry, at) => string(entry, at)),
    binding: { handler: string(binding.handler, 'resource.binding.handler'), cursor },
    log_readers: nodes('log_readers'),
    revision: string(raw.revision, 'resource.revision'),
    event_seq: eventSeq ?? '0',
    retention_ms: (raw.retention_ms as number | undefined) ?? 0,
    ...(raw.data === undefined ? {} : { data: raw.data as Resource['data'] }),
  };
}

export function probeBody(value: unknown): HandoverProbe {
  const raw = shape(value, 'body', ['operation_id', 'resource', 'kind', 'operations', 'metadata', 'initiator', 'cursor', 'deadline']);
  return {
    operation_id: string(raw.operation_id, 'operation_id'),
    resource: resourceRef(raw.resource, 'resource'),
    kind: string(raw.kind, 'kind'),
    operations: list(raw.operations, 'operations', (entry, at) => string(entry, at)),
    ...(raw.metadata === undefined ? {} : { metadata: raw.metadata as Json }),
    initiator: string(raw.initiator, 'initiator'),
    cursor: string(raw.cursor, 'cursor'),
    deadline: string(raw.deadline, 'deadline'),
  };
}

export function operationRequest(request: Request, value: unknown): OperationRequest {
  const raw = shape(value, 'body', ['idempotency_key', 'retry_until', 'operation_id', 'operation', 'expected_revision', 'data']);
  return {
    ...mutation(request, raw),
    operation_id: string(raw.operation_id, 'operation_id'),
    operation: string(raw.operation, 'operation'),
    ...(raw.expected_revision === undefined ? {} : { expected_revision: string(raw.expected_revision, 'expected_revision') }),
    ...(raw.data === undefined ? {} : { data: raw.data as Json }),
  };
}

export function eventBody(value: unknown): { source_id: string; data: Json; expires_at?: string } {
  const raw = shape(value, 'body', ['source_id', 'data', 'expires_at']);
  if (!('data' in raw)) fail('invalid_request', 'data is required.');
  return {
    source_id: string(raw.source_id, 'source_id'),
    data: raw.data as Json,
    ...(raw.expires_at === undefined ? {} : { expires_at: string(raw.expires_at, 'expires_at') }),
  };
}

export function respond(value: unknown, status = 200): Response {
  return new Response(JSON.stringify(value ?? null), {
    status,
    headers: { 'content-type': 'application/json', 'cache-control': 'no-store' },
  });
}

export function expect(request: Request, allowed: string): void {
  if (request.method !== allowed) fail('not_found', `This endpoint does not accept ${request.method}.`);
}
