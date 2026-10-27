import type { AdmissionContext, Auth, ErrorCode, Failure, Mutation, NodeId, NodeOptions, Page, PageOptions, Storage, Transaction, TrustEntry } from './types.js';
import { trustPhase } from './types.js';

const statuses: Record<ErrorCode, number> = {
  invalid_request: 400, unauthenticated: 401, forbidden: 403, not_a_member: 403,
  not_found: 404, id_conflict: 409, revision_conflict: 409, request_in_progress: 409,
  conflict: 409, thread_closed: 410, context_lost: 410, retry_window_expired: 410, cursor_expired: 410,
  payload_too_large: 413, unsupported_media_type: 415, unsupported_delivery: 422,
  ambiguous_target: 422, ambiguous_resource: 422, capacity_exceeded: 429,
  internal: 500, unavailable: 503, decision_unavailable: 503,
};
export class OnlyneError extends Error {
  readonly status: number;
  readonly error: Failure;
  constructor(code: ErrorCode, message: string, details?: Failure['details']) {
    super(message);
    this.status = statuses[code];
    this.error = { code, message, retryable: code === 'unavailable' || code === 'decision_unavailable' || code === 'request_in_progress', ...(details === undefined ? {} : { details }) };
  }
}
export function fail(code: ErrorCode, message: string, details?: Failure['details']): never { throw new OnlyneError(code, message, details); }
export function requireValue<T>(value: T | undefined | null, code: ErrorCode = 'not_found'): T {
  if (value == null) fail(code, 'The requested record is unavailable.');
  return value;
}
export function key(...parts: string[]): string { return parts.map(encodeURIComponent).join('/'); }
/**
 * The durable facts an admission policy decides from, bound to the transaction that is admitting
 * the message (RFC 0017 §4). A policy that opens its own storage transaction deadlocks: the
 * admission transaction still holds the store.
 */
export function admissionView(tx: Transaction): AdmissionContext {
  return {
    async trust(node: NodeId) {
      const entry = await tx.get<TrustEntry>(key('trust', node));
      return entry === undefined ? undefined : trustPhase(entry);
    },
  };
}
export function canonical(value: unknown): string {
  if (value === undefined) return 'null';
  if (value === null || typeof value !== 'object') return JSON.stringify(value);
  if (Array.isArray(value)) return '[' + value.map(canonical).join(',') + ']';
  const record = value as Record<string, unknown>;
  return '{' + Object.keys(record).filter(k => record[k] !== undefined).sort().map(k => JSON.stringify(k) + ':' + canonical(record[k])).join(',') + '}';
}
export function same(a: unknown, b: unknown): boolean { return canonical(a) === canonical(b); }

/** Stable one-way digest of a canonical form. Tombstones keep digests, never content. */
export async function fingerprint(value: unknown): Promise<string> {
  const bytes = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(canonical(value)));
  return Array.from(new Uint8Array(bytes)).map(b => b.toString(16).padStart(2, '0')).join('');
}
export function decimal(value: string): bigint {
  if (!/^(0|[1-9][0-9]*)$/.test(value)) fail('invalid_request', 'Expected a decimal sequence string.');
  return BigInt(value);
}
export function utc(value: string): number {
  if (typeof value !== 'string' || !/^\d{4}-\d\d-\d\dT.*Z$/.test(value) || !Number.isFinite(Date.parse(value))) fail('invalid_request', 'Expected a UTC RFC3339 deadline.');
  return Date.parse(value);
}
export function page<T>(items: T[], options: PageOptions, scope: unknown, position: (item: T) => string, compare: (a: string, b: string) => number = (a, b) => a < b ? -1 : a > b ? 1 : 0): Page<T> {
  const size = options.page_size ?? 50;
  if (!Number.isInteger(size) || size < 1 || size > 100) fail('invalid_request', 'page_size must be from 1 to 100.');
  let after: string | undefined;
  if (options.cursor) {
    let cursor: { scope: unknown; after: string } | null;
    try { cursor = JSON.parse(decodeURIComponent(options.cursor)); } catch { return fail('invalid_request', 'Invalid cursor.'); }
    if (cursor === null || typeof cursor !== 'object' || !same(cursor.scope, scope) || typeof cursor.after !== 'string') fail('invalid_request', 'Cursor belongs to another query.');
    after = cursor.after;
  }
  const remaining = after === undefined ? items : items.filter(item => compare(position(item), after!) > 0);
  const selected = remaining.slice(0, size);
  return { items: selected, next_cursor: remaining.length > size ? encodeURIComponent(canonical({ scope, after: position(selected[selected.length - 1]!) })) : null };
}
export interface Context {
  node: string; baseUrl: string; instanceId: string; storage: Storage; options: NodeOptions;
  now(): number; id(): string;
  owner(owner: string): void;
  deadline(retry_until: string): void;
  mutate<T>(auth: Auth, operation: string, mutation: Mutation, body: unknown, work: (tx: Transaction) => Promise<T>): Promise<T>;
}


export function createContext(options: NodeOptions): Context {
  const ctx: Context = {
    node: options.node_id, baseUrl: options.base_url, instanceId: options.instance_id ?? crypto.randomUUID(), storage: options.plugins.storage, options,
    now: options.now, id: options.id,
    owner(owner) { if (owner !== options.node_id) fail('forbidden', 'This node does not serve the requested owner.'); },
    deadline(until) {
      const time = utc(until);
      if (time <= ctx.now()) fail('retry_window_expired', 'The admission window expired.');
      if (time > ctx.now() + (options.retry_max_ms ?? 86_400_000)) fail('invalid_request', 'The admission window exceeds the supported maximum.');
    },
    async mutate(auth, operation, mutation, body, work) {
      if (!auth.actor) fail('unauthenticated', 'Authentication is required.');
      if (!mutation.idempotency_key) fail('invalid_request', 'An idempotency key is required.');
      const slot = key('idempotency', auth.actor, operation, mutation.idempotency_key);
      const digest = await fingerprint({ body, retry_until: mutation.retry_until });
      return ctx.storage.transaction(async tx => {
        const previous = await tx.get<{ digest: string; result: unknown }>(slot);
        if (previous) {
          if (previous.digest !== digest) fail('id_conflict', 'The idempotency key is bound to another request.');
          return previous.result as never;
        }
        ctx.deadline(mutation.retry_until);
        const result = await work(tx);
        await tx.put(slot, { digest, result });
        return result;
      });
    },
  };
  return ctx;
}
