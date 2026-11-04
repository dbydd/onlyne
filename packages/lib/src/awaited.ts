import { entryKey, isReply, replyKey } from './delivery.ts';
import type { Entry, Message } from './types.ts';

/** One in-flight await on a message this node sent: it lives only while its `send` runs. */
export interface Waiter {
  original: Message;
  expected_author: string;
  /** The sender-local context this await consumes its reply in; absent for a generic caller. */
  local_context?: string;
  outcome: Promise<Entry>;
  settle(entry: Entry): void;
}

/**
 * Live awaits and the tool-result holds they place, one registry per client. A reply already
 * returned as a tool result holds its body in this registry until the host confirms
 * consumption, so no automatic fragment presents the same body in that context again.
 */
export class AwaitedReplies {
  readonly #waiters = new Map<string, Waiter>();
  readonly #held = new Map<string, Set<string>>();

  /** Registered before the network call, so a reply that lands first still finds its waiter. */
  register(record: { request: { message: Message }; expected_author: string; local_context?: string }): Waiter {
    const original = record.request.message;
    const pending = Promise.withResolvers<Entry>();
    const slot = replyKey(original);
    const previous = this.#waiters.get(slot);
    const waiter: Waiter = {
      original,
      expected_author: record.expected_author,
      ...(record.local_context === undefined ? {} : { local_context: record.local_context }),
      outcome: pending.promise,
      settle(entry) {
        previous?.settle(entry);
        pending.resolve(entry);
      },
    };
    this.#waiters.set(slot, waiter);
    return waiter;
  }

  /** An await ends when its caller leaves; a channel replaced by a newer caller stays chained. */
  release(waiter: Waiter): void {
    if (this.#waiters.get(replyKey(waiter.original)) === waiter) this.#waiters.delete(replyKey(waiter.original));
  }

  /** Releases every live await whose original this entry answers; reports how many. */
  settle(entry: Entry): number {
    let settled = 0;
    for (const waiter of this.#waiters.values()) {
      if (waiter.expected_author === entry.authenticated_author && isReply(waiter.original, entry)) {
        waiter.settle(entry);
        settled++;
      }
    }
    return settled;
  }

  /**
   * Whether this entry answers an `await` that owns its body here, optionally only one whose
   * sender-local context is `context`: a waiter still running, or a reply already returned as
   * a tool result and not yet confirmed consumed.
   */
  waiting(entry: Entry, context?: string): boolean {
    for (const waiter of this.#waiters.values()) {
      if (waiter.expected_author !== entry.authenticated_author || !isReply(waiter.original, entry)) continue;
      if (context !== undefined && waiter.local_context !== context) continue;
      return true;
    }
    const held = this.#held.get(entryKey(entry.thread_ref, entry.seq));
    return held !== undefined && (context === undefined || held.has(context));
  }

  /** Holds one entry's body in one context until its consumption is confirmed. */
  hold(entry: Entry, context: string): void {
    const identity = entryKey(entry.thread_ref, entry.seq);
    const held = this.#held.get(identity) ?? new Set<string>();
    this.#held.set(identity, held.add(context));
  }

  /** Drops one context's hold after its path confirmed the entry's consumption. */
  releaseContext(entry: Entry, context: string): void {
    const identity = entryKey(entry.thread_ref, entry.seq);
    const held = this.#held.get(identity);
    if (held === undefined) return;
    held.delete(context);
    if (held.size === 0) this.#held.delete(identity);
  }

  /** Drops every hold on one entry, for the paths that settle it without a confirmation context. */
  forget(entry: Entry): void {
    this.#held.delete(entryKey(entry.thread_ref, entry.seq));
  }
}
