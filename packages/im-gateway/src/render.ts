import type { OnlyneNode, ResourceRef, Storage, ThreadRef } from '@onlyne/lib';
import type { Connector } from '@onlyne/im-core';

/** One configured conversation the renderer watches. */
export interface RenderTarget {
  ref: ResourceRef;
  thread: ThreadRef;
  connector: Connector;
  conversation: string;
}

interface RenderRecord {
  state: 'rendering' | 'rendered';
  at: string;
  message_id?: string;
}

const enc = encodeURIComponent;
const cursorKey = (ref: ResourceRef) => `im/render/${enc(ref.owner)}/${enc(ref.id)}`;
const recordKey = (ref: ResourceRef, seq: string) => `${cursorKey(ref)}/seq/${enc(seq)}`;
const projectionKey = (ref: ResourceRef, messageId: string) => `im/notify/${enc(ref.owner)}/${enc(ref.id)}/${enc(messageId)}`;

/**
 * The reply path back to the platform (RFC 0012 §1: the gateway renders agent replies to the
 * platform). The gateway owns the conversation thread, so it reads the entries a member posted
 * after its render cursor and sends each one out through the connector.
 *
 * The cursor advances only past entries that are settled, so a refusal is retried next pass. An
 * entry whose send was interrupted stays marked `rendering`: the platform cannot deduplicate a
 * re-send (no caller-supplied uuid), so the renderer reports it and moves on instead of risking
 * a duplicate message.
 */
export class ThreadRenderer {
  readonly #node: OnlyneNode;
  readonly #storage: Storage;
  readonly #bridge: { platformHandle(ref: ResourceRef, eventId: string): Promise<{ message_id: string; ref_handle: string } | undefined> };
  readonly #log: (message: string, fields?: Record<string, unknown>) => void;

  constructor(options: {
    node: OnlyneNode;
    storage: Storage;
    bridge: { platformHandle(ref: ResourceRef, eventId: string): Promise<{ message_id: string; ref_handle: string } | undefined> };
    log?: (message: string, fields?: Record<string, unknown>) => void;
  }) {
    this.#node = options.node;
    this.#storage = options.storage;
    this.#bridge = options.bridge;
    this.#log = options.log ?? (() => undefined);
  }

  async pass(targets: RenderTarget[]): Promise<void> {
    for (const target of targets) await this.#renderOne(target);
  }

  async #renderOne(target: RenderTarget): Promise<void> {
    const { ref, thread, connector } = target;
    const cursor = await this.#storage.transaction(async tx =>
      (await tx.get<{ seq: string }>(cursorKey(ref)))?.seq ?? '0');
    let after = cursor;
    for (;;) {
      const page = await this.#node.messages.list(this.#node.auth, thread, {
        attention: 'thread', since_seq: after, page_size: 100,
      });
      if (page.items.length === 0) return;
      for (const entry of page.items) {
        const settled = await this.#renderEntry(target, entry.seq, entry.message);
        if (!settled) return;
        after = entry.seq;
        await this.#storage.transaction(async tx => { await tx.put(cursorKey(ref), { seq: entry.seq }); });
      }
      if (page.next_cursor === null) return;
    }
  }

  /** Returns false when the pass must stop here, so the same entry is retried next round. */
  async #renderEntry(
    target: RenderTarget,
    seq: string,
    message: { id: string; sender: string; msg: string; delivery_kind: string; meta?: { in_reply_to?: { sender: string; id: string } } },
  ): Promise<boolean> {
    const { ref, connector, conversation } = target;
    // The gateway's own projections are the inbound side; they never travel back.
    if (message.sender === this.#node.auth.actor) return true;

    const record = await this.#storage.transaction(async tx => tx.get<RenderRecord>(recordKey(ref, seq)));
    if (record?.state === 'rendered') return true;
    if (record?.state === 'rendering') {
      this.#log('im.render.ambiguous', { ref: `${ref.owner}/${ref.id}`, seq });
      return true;
    }

    const replyTo = await this.#replyHandle(ref, message.meta?.in_reply_to);
    await this.#storage.transaction(async tx => {
      await tx.put(recordKey(ref, seq), { state: 'rendering', at: new Date().toISOString() } satisfies RenderRecord);
    });

    let outcome;
    try {
      outcome = await connector.send({
        conversation,
        text: message.msg,
        format: 'plain',
        attachments: [],
        ...(replyTo === undefined ? {} : { reply_to: replyTo }),
        request_id: message.id,
      });
    } catch (error) {
      // The platform may or may not have taken it; the `rendering` mark keeps a retry from
      // posting a duplicate, and the log is the operator's evidence.
      this.#log('im.render.failed', { ref: `${ref.owner}/${ref.id}`, seq, error: String(error) });
      return true;
    }

    if (outcome.error !== undefined) {
      // An explicit refusal means nothing was sent; drop the mark so the next pass retries.
      await this.#storage.transaction(async tx => { await tx.delete(recordKey(ref, seq)); });
      this.#log('im.render.refused', { ref: `${ref.owner}/${ref.id}`, seq, error: outcome.error });
      return false;
    }

    await this.#storage.transaction(async tx => {
      await tx.put(recordKey(ref, seq), {
        state: 'rendered',
        at: new Date().toISOString(),
        ...(outcome.message_id === undefined ? {} : { message_id: outcome.message_id }),
      } satisfies RenderRecord);
    });
    return true;
  }

  /** A reply to a projection quotes the platform message that projection stands for. */
  async #replyHandle(ref: ResourceRef, original: { sender: string; id: string } | undefined): Promise<string | undefined> {
    if (original === undefined || original.sender !== this.#node.auth.actor) return undefined;
    const eventId = await this.#storage.transaction(async tx => {
      const row = await tx.get<{ event_id: string }>(projectionKey(ref, original.id));
      return row?.event_id;
    });
    if (eventId === undefined) return undefined;
    const handle = await this.#bridge.platformHandle(ref, eventId);
    return handle === undefined ? undefined : handle.ref_handle ?? handle.message_id;
  }
}
