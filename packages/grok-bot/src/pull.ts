import type { Entry, Json, Storage } from '@onlyne/lib';
import {
  DEFAULT_ACK_PATH, DEFAULT_EVENTS_PATH, DEFAULT_REPLY_PATH, webhookHeaderValue, type WebhookConfig,
} from './config.ts';
import { deliveryId, webhookPayload, type FetchLike } from './webhook.ts';

export { DEFAULT_ACK_PATH, DEFAULT_EVENTS_PATH } from './config.ts';
export const DEFAULT_PULL_WAIT_MS = 25_000;
export const DEFAULT_PULL_LEASE_MS = 30_000;
export const DEFAULT_PULL_LIMIT = 16;
export const MAX_PULL_WAIT_MS = 30_000;
const MAX_LIMIT = 50;
/**
 * Bun.serve idleTimeout is seconds (default 10, max 255). A quiet long-poll writes no bytes
 * until it returns, so this must exceed MAX_PULL_WAIT_MS or GET /grok/events is reset.
 */
export const SERVE_IDLE_TIMEOUT_SEC = Math.min(255, Math.ceil(MAX_PULL_WAIT_MS / 1000) + 5);
const PREFIX = 'grok/pull/';

export type PullState = 'owed' | 'leased' | 'acked';

export interface PullRow {
  id: string;
  payload: Json;
  state: PullState;
  attempts: number;
  leased_until: number;
  acked_at?: number;
}

export interface PullItem {
  id: string;
  payload: Json;
  attempts: number;
  leased_until: number;
}

export interface PullStats {
  owed: number;
  leased: number;
  acked: number;
  rows: number;
}

const pullKey = (id: string): string => `${PREFIX}${encodeURIComponent(id)}`;

export function clampWaitMs(value: unknown, fallback = DEFAULT_PULL_WAIT_MS): number {
  if (value === undefined || value === null || value === '') return fallback;
  const parsed = typeof value === 'string' ? Number(value) : value;
  if (typeof parsed !== 'number' || !Number.isFinite(parsed) || parsed < 0) return fallback;
  return Math.min(MAX_PULL_WAIT_MS, Math.floor(parsed));
}

export function clampLimit(value: unknown, fallback = DEFAULT_PULL_LIMIT): number {
  if (value === undefined || value === null || value === '') return fallback;
  const parsed = typeof value === 'string' ? Number(value) : value;
  if (typeof parsed !== 'number' || !Number.isInteger(parsed) || parsed < 1) return fallback;
  return Math.min(MAX_LIMIT, parsed);
}

export function clampLeaseMs(value: unknown, fallback = DEFAULT_PULL_LEASE_MS): number {
  if (value === undefined || value === null || value === '') return fallback;
  const parsed = typeof value === 'string' ? Number(value) : value;
  if (typeof parsed !== 'number' || !Number.isFinite(parsed) || parsed < 1) return fallback;
  return Math.min(300_000, Math.max(1, Math.floor(parsed)));
}

/**
 * Durable pull inbox. Independent of the webhook POST: a NAT device fetches owed
 * payloads and acks them. Unacked leases expire and the same id is offered again
 * (at-least-once). Offline rows stay owed until a client reconnects.
 */
export class PullInbox {
  readonly #storage: Storage;
  readonly #nodeId: string;
  readonly #now: () => number;
  readonly #waiters = new Set<() => void>();

  constructor(options: { storage: Storage; nodeId: string; now?: () => number }) {
    this.#storage = options.storage;
    this.#nodeId = options.nodeId;
    this.#now = options.now ?? Date.now;
  }

  async enqueue(entry: Entry): Promise<void> {
    const id = deliveryId(entry);
    const payload = webhookPayload(this.#nodeId, entry);
    const created = await this.#storage.transaction(async tx => {
      const existing = await tx.get<PullRow>(pullKey(id));
      if (existing !== undefined) return false;
      await tx.put(pullKey(id), {
        id, payload, state: 'owed', attempts: 0, leased_until: 0,
      } satisfies PullRow);
      return true;
    });
    if (created) this.#wake();
  }

  async pull(options: { limit?: number; wait_ms?: number; lease_ms?: number; signal?: AbortSignal } = {}): Promise<PullItem[]> {
    const limit = clampLimit(options.limit);
    const waitMs = clampWaitMs(options.wait_ms, 0);
    const leaseMs = clampLeaseMs(options.lease_ms);
    const deadline = this.#now() + waitMs;
    for (;;) {
      const items = await this.#take(limit, leaseMs);
      if (items.length > 0) return items;
      const remaining = deadline - this.#now();
      if (remaining <= 0) return [];
      const woken = await this.#wait(remaining, options.signal);
      if (!woken) return [];
    }
  }

  async ack(ids: string[]): Promise<{ acked: string[]; missing: string[] }> {
    const unique = [...new Set(ids.filter(id => typeof id === 'string' && id.length > 0))];
    const acked: string[] = [];
    const missing: string[] = [];
    const at = this.#now();
    await this.#storage.transaction(async tx => {
      for (const id of unique) {
        const row = await tx.get<PullRow>(pullKey(id));
        if (row === undefined) {
          missing.push(id);
          continue;
        }
        if (row.state !== 'acked') {
          await tx.put(pullKey(id), { ...row, state: 'acked', acked_at: at } satisfies PullRow);
        }
        acked.push(id);
      }
    });
    return { acked, missing };
  }

  async snapshot(): Promise<PullStats> {
    const now = this.#now();
    const rows = await this.#storage.transaction(async tx => (await tx.scan<PullRow>(PREFIX)).map(row => row.value));
    let owed = 0;
    let leased = 0;
    let acked = 0;
    for (const row of rows) {
      if (row.state === 'acked') acked += 1;
      else if (row.state === 'leased' && row.leased_until > now) leased += 1;
      else owed += 1;
    }
    return { owed, leased, acked, rows: rows.length };
  }

  async #take(limit: number, leaseMs: number): Promise<PullItem[]> {
    const now = this.#now();
    const leasedUntil = now + leaseMs;
    return this.#storage.transaction(async tx => {
      const rows = (await tx.scan<PullRow>(PREFIX)).map(row => row.value)
        .filter(row => row.state !== 'acked' && (row.state !== 'leased' || row.leased_until <= now))
        .sort((a, b) => a.id < b.id ? -1 : a.id > b.id ? 1 : 0);
      const selected = rows.slice(0, limit);
      const items: PullItem[] = [];
      for (const row of selected) {
        const next: PullRow = {
          ...row, state: 'leased', attempts: row.attempts + 1, leased_until: leasedUntil,
        };
        await tx.put(pullKey(row.id), next);
        items.push({ id: next.id, payload: next.payload, attempts: next.attempts, leased_until: next.leased_until });
      }
      return items;
    });
  }

  #wake(): void {
    const waiters = [...this.#waiters];
    this.#waiters.clear();
    for (const wake of waiters) wake();
  }

  #wait(ms: number, signal?: AbortSignal): Promise<boolean> {
    if (signal?.aborted) return Promise.resolve(false);
    return new Promise(resolve => {
      let settled = false;
      const finish = (woken: boolean): void => {
        if (settled) return;
        settled = true;
        this.#waiters.delete(wake);
        signal?.removeEventListener('abort', onAbort);
        clearTimeout(timer);
        resolve(woken);
      };
      const wake = (): void => finish(true);
      const onAbort = (): void => finish(false);
      this.#waiters.add(wake);
      const timer = setTimeout(() => finish(false), ms);
      signal?.addEventListener('abort', onAbort, { once: true });
    });
  }
}

export interface GrokBotPullClientOptions {
  /** Origin of the grok-bot HTTP listener, for example `http://127.0.0.1:3995`. */
  origin: string;
  webhook: Pick<WebhookConfig, 'key' | 'header' | 'scheme'>;
  events_path?: string;
  ack_path?: string;
  reply_path?: string;
  fetch?: FetchLike;
  wait_ms?: number;
  lease_ms?: number;
  limit?: number;
}

export interface GrokBotReplyBody {
  to: string;
  msg: string;
  id?: string;
  delivery_kind?: string;
  thread?: { owner: string; id: string };
  in_reply_to?: { sender: string; id: string };
}

/**
 * Outbound-only client: the device opens every connection. Pull (long-poll) for wakes,
 * ack after handling, POST `/grok/reply` to send. No listener on the device.
 */
export class GrokBotPullClient {
  readonly #origin: string;
  readonly #webhook: Pick<WebhookConfig, 'key' | 'header' | 'scheme'>;
  readonly #eventsPath: string;
  readonly #ackPath: string;
  readonly #replyPath: string;
  readonly #fetch: FetchLike;
  readonly #waitMs: number;
  readonly #leaseMs: number;
  readonly #limit: number;

  constructor(options: GrokBotPullClientOptions) {
    this.#origin = options.origin.replace(/\/+$/, '');
    this.#webhook = options.webhook;
    this.#eventsPath = options.events_path ?? DEFAULT_EVENTS_PATH;
    this.#ackPath = options.ack_path ?? DEFAULT_ACK_PATH;
    this.#replyPath = options.reply_path ?? DEFAULT_REPLY_PATH;
    this.#fetch = options.fetch ?? fetch;
    this.#waitMs = options.wait_ms ?? DEFAULT_PULL_WAIT_MS;
    this.#leaseMs = options.lease_ms ?? DEFAULT_PULL_LEASE_MS;
    this.#limit = options.limit ?? DEFAULT_PULL_LIMIT;
  }

  #headers(json: boolean): Record<string, string> {
    return {
      [this.#webhook.header]: webhookHeaderValue(this.#webhook),
      ...(json ? { 'content-type': 'application/json' } : {}),
    };
  }

  async pull(options: { wait_ms?: number; limit?: number; lease_ms?: number; signal?: AbortSignal } = {}): Promise<PullItem[]> {
    const waitMs = options.wait_ms ?? this.#waitMs;
    const limit = options.limit ?? this.#limit;
    const leaseMs = options.lease_ms ?? this.#leaseMs;
    const url = new URL(this.#eventsPath, `${this.#origin}/`);
    url.searchParams.set('wait_ms', String(waitMs));
    url.searchParams.set('limit', String(limit));
    url.searchParams.set('lease_ms', String(leaseMs));
    const response = await this.#fetch(url, { headers: this.#headers(false), signal: options.signal });
    if (!response.ok) {
      throw new Error(`grok events HTTP ${response.status}`);
    }
    const body = await response.json() as { items?: PullItem[] };
    return Array.isArray(body.items) ? body.items : [];
  }

  async ack(ids: string[]): Promise<{ acked: string[]; missing: string[] }> {
    const response = await this.#fetch(new URL(this.#ackPath, `${this.#origin}/`), {
      method: 'POST',
      headers: this.#headers(true),
      body: JSON.stringify({ ids }),
    });
    if (!response.ok) throw new Error(`grok ack HTTP ${response.status}`);
    const body = await response.json() as { acked?: string[]; missing?: string[] };
    return {
      acked: Array.isArray(body.acked) ? body.acked : [],
      missing: Array.isArray(body.missing) ? body.missing : [],
    };
  }

  async reply(body: GrokBotReplyBody): Promise<{ status: number; body: Json }> {
    const response = await this.#fetch(new URL(this.#replyPath, `${this.#origin}/`), {
      method: 'POST',
      headers: this.#headers(true),
      body: JSON.stringify(body),
    });
    const parsed = await response.json() as Json;
    return { status: response.status, body: parsed };
  }
}
