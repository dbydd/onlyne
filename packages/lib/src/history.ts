import type { Auth, Conversation, Entry, HistoryMessage, HistoryQuery, LocalAttention, MessageRef, Outbound, Page, ThreadRef, Transaction } from './types.ts';
import { fail, key, page, same, type Context } from './core.ts';

const identity = (ref: MessageRef): string => key(ref.sender, ref.id);
const threadIdentity = (ref: ThreadRef): string => key(ref.owner, ref.id);

/** A read-only projection of the records this node already holds. It performs no remote reads. */
export class HistoryService {
  readonly #ctx: Context;

  constructor(ctx: Context) {
    this.#ctx = ctx;
  }

  async list(auth: Auth, query: HistoryQuery = {}): Promise<Page<Conversation>> {
    this.authorize(auth);
    return this.#ctx.storage.transaction(async tx => {
      const conversations = await this.collect(tx);
      const selected = conversations.filter(conversation =>
        (query.thread === undefined || conversation.thread_refs.some(ref => same(ref, query.thread)))
        && (query.message === undefined || conversation.messages.some(row => same(row.message_ref, query.message)) || same(conversation.id, query.message)));
      selected.sort((left, right) => identity(left.id).localeCompare(identity(right.id)));
      return page(selected, query, { kind: 'history', actor: auth.actor, thread: query.thread, message: query.message }, row => identity(row.id));
    });
  }

  async conversation(auth: Auth, message: MessageRef): Promise<Conversation> {
    this.authorize(auth);
    return this.#ctx.storage.transaction(async tx => {
      const found = (await this.collect(tx)).find(row => same(row.id, message) || row.messages.some(item => same(item.message_ref, message)));
      if (found === undefined) fail('not_found', 'This node holds no conversation for that message.');
      return found;
    });
  }

  async message(auth: Auth, message: MessageRef): Promise<HistoryMessage> {
    const conversation = await this.conversation(auth, message);
    const found = conversation.messages.find(row => same(row.message_ref, message));
    if (found === undefined) fail('not_found', 'This node holds the reply reference, but no body for that message.');
    return found;
  }

  private authorize(auth: Auth): void {
    if (auth.actor !== this.#ctx.node) fail('forbidden', 'Conversation history belongs to this node.');
  }

  private async collect(tx: Transaction): Promise<Conversation[]> {
    const messages = new Map<string, HistoryMessage>();
    const addEntry = (entry: Entry, consumed?: boolean): void => {
      const message_ref = { sender: entry.message.sender, id: entry.message.id };
      const id = identity(message_ref);
      const previous = messages.get(id);
      messages.set(id, {
        message_ref, message: entry.message, direction: entry.message.sender === this.#ctx.node ? 'out' : 'in',
        thread_ref: entry.thread_ref, seq: entry.seq, at: entry.at, delivery: previous?.delivery ?? 'received',
        ...(previous?.guarantee === undefined ? {} : { guarantee: previous.guarantee }),
        ...(consumed === undefined ? previous?.consumption === undefined ? {} : { consumption: previous.consumption }
          : { consumption: consumed || previous?.consumption === 'consumed' ? 'consumed' : 'pending' }),
      });
    };
    for (const { value } of await tx.scan<Entry>('entry-ref/')) addEntry(value);
    for (const { value } of await tx.scan<LocalAttention>('inbox/')) {
      if (value.attention.entry !== undefined && value.attention.disposition !== 'expired') addEntry(value.attention.entry, value.injected);
    }
    const confirmed = new Set<string>();
    for (const { value } of await tx.scan<{ thread_ref: ThreadRef; seq: string; confirmed: boolean }>('consumed/')) {
      if (value.confirmed) confirmed.add(key(value.thread_ref.owner, value.thread_ref.id, value.seq));
    }
    for (const { value } of await tx.scan<Outbound>('outbound/')) {
      const message = value.request.message;
      const message_ref = { sender: message.sender, id: message.id };
      const previous = messages.get(identity(message_ref));
      messages.set(identity(message_ref), {
        message_ref, message, direction: 'out', at: value.created_at ?? previous?.at ?? null,
        delivery: value.state,
        ...(value.receipt === undefined ? {} : { thread_ref: value.receipt.thread_ref, seq: value.receipt.seq, guarantee: value.receipt.guarantee }),
        ...(value.error === undefined ? {} : { error: value.error }),
      });
    }
    for (const row of messages.values()) {
      if (row.direction === 'in' && row.thread_ref !== undefined && row.seq !== undefined
        && confirmed.has(key(row.thread_ref.owner, row.thread_ref.id, row.seq))) row.consumption = 'consumed';
    }
    const parents = new Map<string, string>();
    const refs = new Map<string, MessageRef>();
    const root = (id: string): string => {
      const parent = parents.get(id);
      if (parent === undefined || parent === id) return id;
      const found = root(parent);
      parents.set(id, found);
      return found;
    };
    for (const row of messages.values()) {
      const id = identity(row.message_ref);
      refs.set(id, row.message_ref);
      const original = row.message.meta?.in_reply_to;
      if (original === undefined) continue;
      const source = identity(original);
      refs.set(source, original);
      const left = root(id);
      const right = root(source);
      if (left !== right) parents.set(left, right);
    }
    const grouped = new Map<string, HistoryMessage[]>();
    for (const [id, message] of messages) {
      const group = root(id);
      const rows = grouped.get(group) ?? [];
      rows.push(message);
      grouped.set(group, rows);
    }
    return [...grouped].map(([id, rows]) => {
      // Parent-before-child ordering also works when one legacy outbound has no timestamp.
      const ordered: HistoryMessage[] = [];
      const seen = new Set<string>();
      const byId = new Map(rows.map(row => [identity(row.message_ref), row]));
      const visit = (row: HistoryMessage): void => {
        const key = identity(row.message_ref);
        if (seen.has(key)) return;
        seen.add(key);
        const source = row.message.meta?.in_reply_to;
        const parent = source === undefined ? undefined : byId.get(identity(source));
        if (parent !== undefined) visit(parent);
        ordered.push(row);
      };
      rows.sort((a, b) => (a.at ?? '').localeCompare(b.at ?? '') || identity(a.message_ref).localeCompare(identity(b.message_ref)));
      rows.forEach(visit);
      const threads = new Map<string, ThreadRef>();
      for (const row of ordered) if (row.thread_ref !== undefined) threads.set(threadIdentity(row.thread_ref), row.thread_ref);
      const subject = ordered[0]!.message.delivery_kind.replace(/\s+/g, ' ').trim();
      return { id: refs.get(id)!, title: subject || ordered[0]!.message.msg.split(/\r?\n/, 1)[0]!, thread_refs: [...threads.values()], messages: ordered, last_at: rows.filter(row => row.at !== null).at(-1)?.at ?? null };
    });
  }
}
