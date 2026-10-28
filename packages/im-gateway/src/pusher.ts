import type { Json, NodeId, OnlyneClient, ResourceEvent, ResourceRef, Storage, ThreadRef } from '@onlyne/lib';
import { leveled, type LeveledLog, type LogSink } from '@onlyne/node-host';

/** One event whose notify message has not been confirmed delivered. */
interface OwedPush {
  ref: ResourceRef;
  event: ResourceEvent;
  /**
   * The projection message id, fixed when the row was written. The id embeds the endpoint's
   * identity, so a retry after an owner migration must reuse it instead of minting a second
   * identity for a push some peer may already have accepted (offline migration, migrate.ts).
   */
  message_id?: string;
  /** Attempts already made, so the log names this one instead of restarting at one. */
  attempts?: number;
}

/** How one delivery attempt ended, in the push log's vocabulary. */
type PushOutcome = 'accepted' | 'refused' | 'timeout' | 'error';

/** The deterministic projection identity for one received event. */
export function notifyMessageId(ref: ResourceRef, eventId: string): string {
  return `im-notify-${ref.owner}-${ref.id}-${eventId}`;
}

const owedKey = (ref: ResourceRef, eventId: string) =>
  `im/push/${encodeURIComponent(ref.owner)}/${encodeURIComponent(ref.id)}/${encodeURIComponent(eventId)}`;
/** Which event a projection message stands for, so a reply to it can quote the platform message. */
const projectionKey = (ref: ResourceRef, messageId: string) =>
  `im/notify/${encodeURIComponent(ref.owner)}/${encodeURIComponent(ref.id)}/${encodeURIComponent(messageId)}`;

/** The body an assignee reads in the thread: the platform text, or what kind of event it was. */
function bodyOf(data: Record<string, Json>): string {
  if (typeof data.text === 'string' && data.text.length > 0) return data.text;
  if (data.kind === 'event') {
    const event = data.event as Record<string, Json> | undefined;
    return typeof event?.type === 'string' ? `[${event.type}]` : '[event]';
  }
  if (data.kind === 'action') {
    const action = data.action as Record<string, Json> | undefined;
    return typeof action?.id === 'string' ? `[action ${action.id}]` : '[action]';
  }
  return '[message]';
}

/**
 * The push loop. After each accepted `receive` the gateway sends the resource's current handler a
 * message into the conversation's own thread (RFC 0012 §1: one thread per platform conversation,
 * owned by the gateway), so the handler reads the platform body in place and the thread's `seq`
 * grows with arrival order. An event carries no recipient (RFC 0006 §5), so every attempt resolves
 * the handler again and a handover moves the push target with the binding. The attempt is recorded
 * before sending, so a failed push is retried by the maintenance ring until the record is dropped.
 */
export class EventPusher {
  readonly #client: () => OnlyneClient;
  readonly #storage: Storage;
  readonly #sender: string;
  readonly #threadOf: (ref: ResourceRef) => ThreadRef | undefined;
  readonly #handlerOf: (ref: ResourceRef) => Promise<NodeId | undefined>;
  readonly #log: LeveledLog;
  #attempts = 0;
  #failures = 0;

  constructor(options: {
    client: () => OnlyneClient;
    storage: Storage;
    sender: string;
    threadOf?: (ref: ResourceRef) => ThreadRef | undefined;
    handlerOf?: (ref: ResourceRef) => Promise<NodeId | undefined>;
    log?: LogSink;
  }) {
    this.#client = options.client;
    this.#storage = options.storage;
    this.#sender = options.sender;
    this.#threadOf = options.threadOf ?? (() => undefined);
    this.#handlerOf = options.handlerOf ?? (async () => undefined);
    this.#log = leveled(options.log);
  }

  /** Attempt and failure totals so far, for the maintenance summary. */
  stats(): { attempts: number; failures: number } {
    return { attempts: this.#attempts, failures: this.#failures };
  }

  /** Records the owed push before sending, then attempts it. A failure stays owed for the ring. */
  async afterReceive(ref: ResourceRef, event: ResourceEvent): Promise<void> {
    const message_id = notifyMessageId(ref, event.event_id);
    await this.#storage.transaction(async tx => {
      await tx.put(owedKey(ref, event.event_id), { ref, event, message_id, attempts: 0 } satisfies OwedPush);
    });
    await this.deliverOne(ref, event.event_id);
  }

  /** One attempt; the owed row is deleted only when the notify message was accepted. */
  async deliverOne(ref: ResourceRef, eventId: string): Promise<void> {
    const client = this.#client();
    const stored = await this.#storage.transaction(async tx =>
      tx.get<OwedPush>(owedKey(ref, eventId)));
    const attempt = (stored?.attempts ?? 0) + 1;
    if (stored !== undefined) {
      await this.#storage.transaction(async tx => {
        await tx.put(owedKey(ref, eventId), { ...stored, attempts: attempt } satisfies OwedPush);
      });
    }
    let target: NodeId | undefined;
    let seq: string | undefined;
    /** One line per attempt: the outcome vocabulary is the same for the ring and the first send. */
    const record = (outcome: PushOutcome, error?: string): void => {
      this.#attempts += 1;
      if (outcome !== 'accepted') this.#failures += 1;
      const fields = {
        target: target ?? null,
        ...(seq === undefined ? {} : { seq }),
        event_id: eventId,
        outcome,
        attempt,
        ...(error === undefined ? {} : { error }),
      };
      if (outcome === 'accepted') this.#log.log('resource.push', fields);
      else this.#log.warn('resource.push', fields);
    };
    try {
      const thread = this.#threadOf(ref);
      if (thread === undefined) {
        // Registration creates the thread before connectors start, so this is a misconfigured
        // resource rather than a transient fault; the owed row stays and is retried.
        record('error', 'no thread');
        return;
      }
      const recipient = await this.#handlerOf(ref);
      if (recipient === undefined) {
        record('error', 'no handler');
        return;
      }
      target = recipient;
      const data = (stored?.event.data ?? {}) as Record<string, Json>;
      const messageId = stored?.message_id ?? notifyMessageId(ref, eventId);
      const outcome = await client.send(
        recipient,
        {
          id: messageId,
          sender: this.#sender,
          delivery_kind: 'notify',
          msg: bodyOf(data),
          meta: {
            thread,
            data: {
              resource_ref: { owner: ref.owner, id: ref.id },
              event_id: eventId,
              source_id: stored?.event.source_id ?? '',
              ...(typeof data.platform === 'string' ? { platform: data.platform } : {}),
              ...(typeof data.conversation === 'string' ? { conversation: data.conversation } : {}),
              ...(typeof data.from === 'string' ? { from: data.from } : {}),
              ...(data.reply === undefined ? {} : { reply: data.reply }),
              // The projection carries the whole platform payload the assignee needs: an
              // attachment list, a card action, or a lifecycle event, so it need not read the
              // resource event back for the common cases.
              ...(Array.isArray(data.attachments) ? { attachments: data.attachments } : {}),
              ...(data.action === undefined ? {} : { action: data.action }),
              ...(data.event === undefined ? {} : { event: data.event }),
            },
          },
        },
        { response: 'none', execution: 'existing' },
      );
      seq = outcome.receipt?.seq;
      // `client.send` reports a refused delivery instead of throwing; only an accepted
      // notify clears the owed row, so the ring retries the rest.
      if (outcome.error === undefined) {
        await this.#storage.transaction(async tx => {
          await tx.put(projectionKey(ref, messageId), { event_id: eventId });
          await tx.delete(owedKey(ref, eventId));
        });
        record('accepted');
      } else {
        // An unreachable peer gave no answer; any other failure is the peer's own refusal.
        record(outcome.error.code === 'unavailable' ? 'timeout' : 'refused', outcome.error.message);
      }
    } catch (error) {
      record('error', String(error));
    }
  }

  /** The maintenance ring's pass: every owed row gets one attempt, in event order. */
  async retryOwed(): Promise<void> {
    const rows = await this.#storage.transaction(async tx =>
      (await tx.scan<OwedPush>('im/push/')).map(row => row.value));
    rows.sort((a, b) => (a.event.at < b.event.at ? -1 : a.event.at > b.event.at ? 1 : 0));
    for (const row of rows) await this.deliverOne(row.ref, row.event.event_id);
  }
}
