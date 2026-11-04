import type { Entry, Json, NodeId, Storage, ThreadRef } from '@onlyne/lib';
import { leveled, type LeveledLog, type LogSink } from '@onlyne/node-host';
import { redactWebhook, webhookHeaderValue, type WebhookConfig } from './config.ts';
import { grokBotChatFromThread } from './threads.ts';

export type FetchLike = (input: string | URL | Request, init?: RequestInit) => Promise<Response>;

/** One inbound message whose webhook POST has not been confirmed. */
export interface OwedWebhook {
  id: string;
  payload: Json;
  attempts: number;
  next_attempt_at: number;
  state: 'owed' | 'delivered' | 'failed';
  last_error?: string;
  last_status?: number;
}

export interface WebhookStats {
  owed: number;
  delivered: number;
  failed: number;
  attempts: number;
  in_flight: number;
}

const PREFIX = 'grok/webhook/';
const MAX_BACKOFF_MS = 60_000;

const owedKey = (id: string): string => `${PREFIX}${encodeURIComponent(id)}`;

export function deliveryId(entry: Entry): string {
  return `${entry.message.sender}/${entry.message.id}`;
}

/** URL shape safe to log: origin and path, never query or fragment. */
export function urlForLog(url: string): string {
  try {
    const parsed = new URL(url);
    return `${parsed.origin}${parsed.pathname}`;
  } catch {
    return '(invalid url)';
  }
}

function classify(status: number): 'ok' | 'retry' | 'fail' {
  if (status >= 200 && status < 300) return 'ok';
  if (status === 408 || status === 429 || status >= 500) return 'retry';
  return 'fail';
}

function backoff(base: number, attempts: number): number {
  const shift = Math.min(attempts - 1, 16);
  return Math.min(MAX_BACKOFF_MS, base * (2 ** Math.max(0, shift)));
}

/** JSON body Grok Bot receives for one inbound Onlyne message. */
export function webhookPayload(nodeId: NodeId, entry: Entry): Json {
  const chat = grokBotChatFromThread(entry.thread_ref, entry.message.sender);
  const grokbot = { chat_kind: chat.kind, chat_id: chat.chat_id };
  const originalMeta = (entry.message.meta ?? {}) as Record<string, Json>;
  const meta = { ...originalMeta, grokbot } as Json;
  const message = {
    id: entry.message.id,
    sender: entry.message.sender,
    delivery_kind: entry.message.delivery_kind,
    msg: entry.message.msg,
    meta,
  };
  return {
    node_id: nodeId,
    sender: entry.message.sender,
    authenticated_author: entry.authenticated_author,
    reply_authority: entry.reply_authority,
    message_id: entry.message.id,
    timestamp: entry.at,
    thread: { owner: entry.thread_ref.owner, id: entry.thread_ref.id },
    seq: entry.seq,
    delivery_kind: entry.message.delivery_kind,
    msg: entry.message.msg,
    message,
    meta,
  };
}

/**
 * Durable webhook queue. An inbound message is recorded before the POST, so a crash or a
 * non-2xx leaves the row owed. Each row retries on its own schedule; a stuck delivery never
 * blocks another message.
 */
export class WebhookDispatcher {
  readonly #storage: Storage;
  readonly #webhook: WebhookConfig;
  readonly #nodeId: NodeId;
  readonly #fetch: FetchLike;
  readonly #now: () => number;
  readonly #log: LeveledLog;
  readonly #inFlight = new Set<string>();
  #attempts = 0;
  #delivered = 0;
  #failed = 0;

  constructor(options: {
    storage: Storage;
    webhook: WebhookConfig;
    nodeId: NodeId;
    fetch?: FetchLike;
    now?: () => number;
    log?: LogSink;
  }) {
    this.#storage = options.storage;
    this.#webhook = options.webhook;
    this.#nodeId = options.nodeId;
    this.#fetch = options.fetch ?? fetch;
    this.#now = options.now ?? Date.now;
    this.#log = leveled(options.log);
  }

  stats(): WebhookStats {
    return {
      owed: -1,
      delivered: this.#delivered,
      failed: this.#failed,
      attempts: this.#attempts,
      in_flight: this.#inFlight.size,
    };
  }

  async snapshot(): Promise<{ webhook: ReturnType<typeof redactWebhook>; queue: WebhookStats & { rows: number } }> {
    const rows = await this.#storage.transaction(async tx => (await tx.scan<OwedWebhook>(PREFIX)).map(row => row.value));
    const owed = rows.filter(row => row.state === 'owed').length;
    const failed = rows.filter(row => row.state === 'failed').length;
    const delivered = rows.filter(row => row.state === 'delivered').length;
    return {
      webhook: redactWebhook(this.#webhook),
      queue: {
        owed, failed, delivered, attempts: this.#attempts, in_flight: this.#inFlight.size, rows: rows.length,
      },
    };
  }

  /** Records the owed POST (idempotent on message identity) and starts one attempt without waiting. */
  enqueue(entry: Entry): void {
    void this.#enqueue(entry);
  }

  async #enqueue(entry: Entry): Promise<void> {
    const id = deliveryId(entry);
    const payload = webhookPayload(this.#nodeId, entry);
    const created = await this.#storage.transaction(async tx => {
      const existing = await tx.get<OwedWebhook>(owedKey(id));
      if (existing !== undefined) return existing.state === 'owed';
      await tx.put(owedKey(id), {
        id, payload, attempts: 0, next_attempt_at: this.#now(), state: 'owed',
      } satisfies OwedWebhook);
      return true;
    });
    if (created) this.#log.log('grok.webhook.queued', { id, url: urlForLog(this.#webhook.url) });
    void this.deliverOne(id);
  }

  /** One attempt for one row. Concurrent calls for the same id share the in-flight slot. */
  async deliverOne(id: string): Promise<void> {
    if (this.#inFlight.has(id)) return;
    this.#inFlight.add(id);
    try {
      const row = await this.#storage.transaction(async tx => tx.get<OwedWebhook>(owedKey(id)));
      if (row === undefined || row.state !== 'owed') return;
      if (row.next_attempt_at > this.#now()) return;
      const attempt = row.attempts + 1;
      await this.#storage.transaction(async tx => {
        const current = await tx.get<OwedWebhook>(owedKey(id));
        if (current === undefined || current.state !== 'owed') return;
        await tx.put(owedKey(id), { ...current, attempts: attempt } satisfies OwedWebhook);
      });
      this.#attempts += 1;
      const outcome = await this.#post(row.payload);
      if (outcome.kind === 'ok') {
        await this.#storage.transaction(async tx => {
          const current = await tx.get<OwedWebhook>(owedKey(id));
          if (current === undefined) return;
          await tx.put(owedKey(id), { ...current, state: 'delivered', last_status: outcome.status } satisfies OwedWebhook);
        });
        this.#delivered += 1;
        this.#log.log('grok.webhook.delivered', { id, attempt, status: outcome.status });
        return;
      }
      const retryable = outcome.kind === 'retry' && attempt < this.#webhook.max_attempts;
      const next = this.#now() + backoff(this.#webhook.retry_base_ms, attempt);
      const state = retryable ? 'owed' : 'failed';
      await this.#storage.transaction(async tx => {
        const current = await tx.get<OwedWebhook>(owedKey(id));
        if (current === undefined || current.state === 'delivered') return;
        await tx.put(owedKey(id), {
          ...current,
          state,
          next_attempt_at: next,
          last_error: outcome.error,
          ...(outcome.status === undefined ? {} : { last_status: outcome.status }),
        } satisfies OwedWebhook);
      });
      if (state === 'failed') this.#failed += 1;
      const fields = {
        id, attempt, error: outcome.error,
        ...(outcome.status === undefined ? {} : { status: outcome.status }),
        ...(retryable ? { next_attempt_at: new Date(next).toISOString() } : { exhausted: true }),
      };
      if (retryable) this.#log.warn('grok.webhook.retry', fields);
      else this.#log.error('grok.webhook.failed', fields);
    } catch (error) {
      this.#log.error('grok.webhook.failed', { id, error: String(error) });
    } finally {
      this.#inFlight.delete(id);
    }
  }

  /** One maintenance pass: every owed row whose backoff has elapsed gets one attempt, in parallel. */
  async retryOwed(): Promise<void> {
    const now = this.#now();
    const rows = await this.#storage.transaction(async tx =>
      (await tx.scan<OwedWebhook>(PREFIX)).map(row => row.value));
    const due = rows.filter(row => row.state === 'owed' && row.next_attempt_at <= now);
    await Promise.all(due.map(row => this.deliverOne(row.id)));
  }

  async #post(payload: Json): Promise<{ kind: 'ok' | 'retry' | 'fail'; status?: number; error: string }> {
    const headerValue = webhookHeaderValue(this.#webhook);
    try {
      const response = await this.#fetch(this.#webhook.url, {
        method: 'POST',
        headers: {
          'content-type': 'application/json',
          [this.#webhook.header]: headerValue,
        },
        body: JSON.stringify(payload),
        signal: AbortSignal.timeout(this.#webhook.timeout_ms),
      });
      const kind = classify(response.status);
      if (kind === 'ok') return { kind, status: response.status, error: '' };
      return { kind, status: response.status, error: `HTTP ${response.status}` };
    } catch (error) {
      const text = error instanceof Error ? error.message : String(error);
      const timeout = error instanceof Error && (error.name === 'TimeoutError' || error.name === 'AbortError');
      return { kind: 'retry', error: timeout ? `timeout after ${this.#webhook.timeout_ms}ms` : text };
    }
  }
}

export interface EntryLookup {
  list(auth: { actor: NodeId }, ref: ThreadRef, query: { attention: 'thread'; since_seq?: string; page_size: number }): Promise<{ items: Entry[] }>;
  inbox(auth: { actor: NodeId }): Promise<{ items: Array<{ attention: { thread_ref: ThreadRef; seq: string; entry?: Entry } }> }>;
}

/** Resolves the notify triple (thread, seq, sender) to a stored entry, then wakes consumers. */
export async function enqueueNotify(
  wake: (entry: Entry) => void,
  lookup: EntryLookup,
  auth: { actor: NodeId },
  event: { thread: ThreadRef; seq: string; sender: NodeId },
  log: LeveledLog,
): Promise<void> {
  const since = event.seq === '1' ? undefined : (BigInt(event.seq) - 1n).toString();
  try {
    const listed = await lookup.list(auth, event.thread, {
      attention: 'thread', page_size: 5, ...(since === undefined ? {} : { since_seq: since }),
    });
    const fromList = listed.items.find(entry => entry.seq === event.seq);
    if (fromList !== undefined) {
      wake(fromList);
      return;
    }
    const inbox = await lookup.inbox(auth);
    const fromInbox = inbox.items.find(row =>
      row.attention.seq === event.seq
      && row.attention.thread_ref.owner === event.thread.owner
      && row.attention.thread_ref.id === event.thread.id)?.attention.entry;
    if (fromInbox !== undefined) {
      wake(fromInbox);
      return;
    }
    log.warn('grok.webhook.missing', { thread: event.thread.id, seq: event.seq, sender: event.sender });
  } catch (error) {
    log.warn('grok.webhook.lookup', { thread: event.thread.id, seq: event.seq, error: String(error) });
  }
}
