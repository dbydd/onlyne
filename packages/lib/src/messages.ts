import type { Attention, Auth, DeliveryPlan, Entry, Event, Failure, Grant, Json, LocalAttention, Member, MessageQuery, MessageRef, Mutation, PageOptions, Receipt, ReplyPermit, SendRequest, Thread, ThreadRef, Transaction } from './types.js';
import { canonical, decimal, fail, fingerprint, key, OnlyneError, page, requireValue, same, type Context } from './core.js';
import { canRead, getThread } from './threads.js';

type Remembered = { digest: string; receipt?: Receipt; error?: Failure; retry_until: string };
type Binding = { digest: string; identity: string };
type Outcome = { receipt: Receipt } | { error: Failure };

/** Refusals this receiver records durably; retryable, internal and capacity failures roll back instead. */
const RECORDED_REFUSALS: Record<string, true> = {
  invalid_request: true, forbidden: true, not_a_member: true, not_found: true,
  id_conflict: true, revision_conflict: true, conflict: true, thread_closed: true,
  retry_window_expired: true, payload_too_large: true, unsupported_media_type: true,
  unsupported_delivery: true, ambiguous_target: true, ambiguous_resource: true,
};

/** Cursors and positions in this service are decimal strings; compare them as numbers, never as text. */
const compareOrdinal = (a: string, b: string): number => decimal(a) < decimal(b) ? -1 : decimal(a) > decimal(b) ? 1 : 0;

export class MessageService {
  constructor(private readonly ctx: Context) {}

  async admit(auth: Auth, request: SendRequest): Promise<Receipt> {
    const { message } = request;
    if (!auth.actor) fail('unauthenticated', 'Authentication is required.');
    if (typeof message?.id !== 'string' || !message.id || typeof message.sender !== 'string' || typeof message.msg !== 'string' || typeof message.delivery_kind !== 'string') fail('invalid_request', 'A message needs id, sender, delivery_kind and msg.');
    if (message.sender !== auth.actor && auth.sender !== message.sender) fail('forbidden', 'The caller cannot represent this author.');
    if (request.target_endpoint !== this.ctx.node) fail('forbidden', 'The target endpoint is not served here.');
    if (!request.plan || !['none', 'async', 'await'].includes(request.plan.response) || !['existing', 'new'].includes(request.plan.execution)) fail('invalid_request', 'An executable delivery plan is required.');
    if (!request.idempotency_key) fail('invalid_request', 'An idempotency key is required.');
    if (message.meta?.relation && !message.meta.in_reply_to) fail('invalid_request', 'A relation requires an original message.');
    const operationKey = key('message-key', auth.actor, request.idempotency_key);
    const requestDigest = await fingerprint({ message, target_endpoint: request.target_endpoint, plan: request.plan, retry_until: request.retry_until });
    const identity = key('message', message.sender, message.id);
    const digest = await fingerprint({ message, target_endpoint: request.target_endpoint });

    const outcome = await this.ctx.storage.transaction(async (tx): Promise<Outcome> => {
      const bound = await tx.get<Binding>(operationKey);
      if (bound && (bound.digest !== requestDigest || bound.identity !== identity)) fail('id_conflict', 'The idempotency key is bound to another request.');
      const old = await tx.get<Remembered>(identity);
      if (old) {
        this.ctx.options.plugins.runtime?.trace?.({ event: 'inbound.duplicate', sender: message.sender, message: message.id });
        // A different body never replaces the identity already recorded, accepted or refused.
        if (old.digest !== digest) return { error: new OnlyneError('id_conflict', 'This message identity has different content.').error };
        if (old.error) return { error: old.error };
        if (old.receipt === undefined) return { error: new OnlyneError('retry_window_expired', 'Only the message tombstone remains.').error };
        if (bound === undefined) await tx.put(operationKey, { digest: requestDigest, identity });
        return { receipt: old.receipt };
      }
      try {
        const runtime = this.ctx.options.plugins.runtime;
        if (runtime && !await runtime.supports(request.plan)) {
          return await this.refuse(tx, identity, digest, request, new OnlyneError('unsupported_delivery', 'The runtime cannot fulfill this delivery plan.'));
        }
        this.ctx.deadline(request.retry_until);
        return await this.append(tx, auth, request, digest, requestDigest, identity, operationKey);
      } catch (error) {
        if (error instanceof OnlyneError && RECORDED_REFUSALS[error.error.code] === true) return this.refuse(tx, identity, digest, request, error);
        throw error;
      }
    });
    if ('error' in outcome) throw new OnlyneError(outcome.error.code, outcome.error.message, outcome.error.details);
    this.ctx.options.plugins.runtime?.notify?.({ event: 'inbound', thread: outcome.receipt.thread_ref, seq: outcome.receipt.seq, sender: message.sender });
    this.ctx.options.plugins.runtime?.notify?.({ event: 'work-pending', pending: true });
    return requireValue(outcome.receipt);
  }

  /**
   * Every rejecting check runs before the first write, so a refusal commits only its
   * durable refusal record and never a half-appended entry. A mention, a closed thread,
   * a missing membership or a policy refusal all leave the thread cursor untouched.
   *
   * A first contact the owner's inbound policy permits stages its thread, sender
   * membership and grant notice, then writes them beside the entry and the receipt in
   * this same transaction: one admission records all of them or none.
   */
  private async append(tx: Transaction, auth: Auth, request: SendRequest, digest: string, requestDigest: string, identity: string, operationKey: string): Promise<Outcome> {
    const { message } = request;
    const original = message.meta?.in_reply_to;
    let threadRef = message.meta?.thread;
    let permit: ReplyPermit | undefined;
    if (original !== undefined) {
      permit = await tx.get<ReplyPermit>(key('permit', original.sender, original.id, message.sender));
      if (threadRef === undefined && permit && !permit.revoked && permit.remaining > 0 && Date.parse(permit.expires_at) > this.ctx.now()
        && (message.meta?.relation !== 'followup' || permit.followup) && this.ctx.node === permit.landing.owner) threadRef = permit.landing;
    }
    let firstContact = false;
    if (threadRef === undefined) {
      // The default landing is the owner's own slot, resolved once inside this transaction.
      const landing = await tx.get<ThreadRef>(key('default', this.ctx.node));
      if (landing === undefined || landing.owner !== this.ctx.node || typeof landing.id !== 'string' || landing.id.length === 0) {
        // Finding a landing grants nothing (RFC 0002 §3): only the owner's inbound policy
        // may open a thread and admit this sender into it.
        const policy = this.ctx.options.plugins.inboundThread;
        if (policy === undefined || !await policy(auth.actor)) {
          return this.refuse(tx, identity, digest, request, new OnlyneError('not_found', 'No default thread is configured.'));
        }
        firstContact = true;
        let id = crypto.randomUUID();
        while (await tx.get<Thread>(key('thread', this.ctx.node, id))) id = crypto.randomUUID();
        threadRef = { owner: this.ctx.node, id };
      } else {
        threadRef = { owner: landing.owner, id: landing.id };
      }
    }
    this.ctx.owner(threadRef.owner);
    const thread: Thread = firstContact
      ? { thread_ref: threadRef, title: `first contact from ${message.sender}`, state: 'open', next_seq: '1', event_seq: '0', created_at: new Date(this.ctx.now()).toISOString(), closed_at: null }
      : await getThread(tx, this.ctx, threadRef);
    const member = firstContact ? undefined : await tx.get<Member>(key('member', threadRef.owner, threadRef.id, message.sender));
    const ordinary = firstContact || message.sender === threadRef.owner || (member?.state === 'active' && member.access.includes('post'));
    const allowedPermit = permit !== undefined && !permit.revoked && permit.remaining > 0 && Date.parse(permit.expires_at) > this.ctx.now()
      && same(permit.landing, threadRef) && (message.meta?.relation !== 'followup' || permit.followup);
    if (!ordinary && !allowedPermit) return this.refuse(tx, identity, digest, request, new OnlyneError('not_a_member', 'The sender is not an active post member.'));
    if (thread.state === 'closed') return this.refuse(tx, identity, digest, request, new OnlyneError('thread_closed', 'The thread is closed.'));
    const admissionPolicy = this.ctx.options.plugins.mayAdmit;
    if (admissionPolicy && !await admissionPolicy(auth.actor, threadRef)) {
      return this.refuse(tx, identity, digest, request, new OnlyneError('forbidden', 'The admission policy refused the request.'));
    }

    const members = (await tx.scan<Member>(key('member', threadRef.owner, threadRef.id) + '/')).map(row => row.value).filter(m => m.state === 'active');
    const recipients = new Set<string>();
    // A direct first contact is the owner's business even without a mention.
    if (firstContact) recipients.add(threadRef.owner);
    for (const mention of message.meta?.mentions ?? []) {
      if (typeof mention !== 'string' || mention.length === 0) fail('invalid_request', 'A mention must be a non-empty string.');
      if (mention === '@all') {
        recipients.add(threadRef.owner);
        members.forEach(m => recipients.add(m.peer));
        continue;
      }
      const alias = mention.startsWith('@') ? mention.slice(1) : mention;
      // The owner is addressable by its own node id without a membership record.
      if (alias === threadRef.owner) { recipients.add(threadRef.owner); continue; }
      if (members.some(m => m.peer === alias)) { recipients.add(alias); continue; }
      // A display alias is not a node id; the integration layer owns the mapping.
      const resolved = await this.ctx.options.plugins.mention?.(alias, threadRef);
      if (resolved === undefined || (resolved !== threadRef.owner && !members.some(m => m.peer === resolved))) {
        fail('invalid_request', 'A mention must resolve to an active member identity.');
      }
      recipients.add(resolved);
    }
    if (original !== undefined) {
      const source = await tx.get<Entry>(key('entry-ref', original.sender, original.id));
      recipients.add(source ? source.reply_authority : original.sender);
    }

    if (firstContact) {
      // Written with the entry and the receipt in this one transaction: the thread, its
      // sender membership and the grant notice either all commit or none of them do.
      const revision = this.ctx.id();
      const membership: Member = { peer: message.sender, state: 'active', access: ['post', 'read'], read_scope: {}, revision };
      const notice: Grant = { id: revision, recipient: message.sender, thread_ref: threadRef, revision, acked: false };
      const eventId = (decimal(thread.event_seq) + 1n).toString();
      thread.event_seq = eventId;
      await tx.put(key('member', threadRef.owner, threadRef.id, message.sender), membership);
      await tx.put(key('notice', notice.id), notice);
      await tx.put(key('event', threadRef.owner, threadRef.id, eventId), { id: eventId, type: 'membership', thread_ref: threadRef, data: JSON.parse(JSON.stringify(membership)) as Json } satisfies Event);
    }
    const seq = thread.next_seq;
    thread.next_seq = (decimal(seq) + 1n).toString();
    thread.event_seq = (decimal(thread.event_seq) + 1n).toString();
    const entry: Entry = {
      thread_ref: threadRef, seq, message: structuredClone(message), authenticated_author: message.sender,
      reply_authority: auth.reply_authority ?? auth.actor, at: new Date(this.ctx.now()).toISOString(),
    };
    const receipt: Receipt = {
      message_ref: { sender: message.sender, id: message.id }, receipt_id: this.ctx.id(),
      guarantee: this.ctx.storage.guarantee, thread_ref: threadRef, seq, plan: structuredClone(request.plan),
    };
    await tx.put(key('thread', threadRef.owner, threadRef.id), thread);
    await tx.put(key('entry', threadRef.owner, threadRef.id, seq), entry);
    await tx.put(key('entry-ref', message.sender, message.id), entry);
    await tx.put<Remembered>(identity, { digest, receipt, retry_until: request.retry_until });
    await tx.put(operationKey, { digest: requestDigest, identity });
    if (permit !== undefined && original !== undefined && !ordinary && allowedPermit) {
      permit.remaining--;
      await tx.put(key('permit', original.sender, original.id, message.sender), permit);
    }
    for (const recipient of recipients) {
      const obligation_id = key(threadRef.owner, threadRef.id, message.sender, message.id, recipient);
      await this.writeAttention(tx, {
        obligation_id, recipient, thread_ref: threadRef, seq,
        message_ref: { sender: message.sender, id: message.id }, authenticated_author: message.sender, acked: false, entry,
      }, request.plan);
    }
    const event: Event = { id: thread.event_seq, type: 'message', thread_ref: threadRef, message_ref: receipt.message_ref, data: { seq } };
    await tx.put(key('event', threadRef.owner, threadRef.id, event.id), event);
    return { receipt };
  }

  /** Terminal refusals are the receiver's record too: written through the admission transaction. */
  private async refuse(tx: Transaction, identity: string, digest: string, request: SendRequest, error: OnlyneError): Promise<Outcome> {
    const outcome = error.error;
    await tx.put<Remembered>(identity, { digest, error: outcome, retry_until: request.retry_until });
    return { error: outcome };
  }

  private async writeAttention(tx: Transaction, attention: Attention, plan: DeliveryPlan) {
    const attested: Attention = { ...attention, plan };
    await tx.put(key('attention', attested.thread_ref.owner, attested.thread_ref.id, attested.obligation_id), attested);
    if (attested.recipient !== this.ctx.node) return;
    const slot = key('inbox', attested.obligation_id);
    if (await tx.get(slot) !== undefined) return;
    await tx.put<LocalAttention>(slot, {
      attention: attested,
      ...(plan.context === undefined ? {} : { context: plan.context }),
      injected: false,
    });
    this.ctx.options.plugins.runtime?.trace?.({
      event: 'inbound.received', obligation: attested.obligation_id, thread: attested.thread_ref,
      seq: attested.seq, sender: attested.message_ref.sender, valid: true, injected: false,
      ...(plan.context === undefined ? {} : { context: plan.context }),
      ...(attested.entry?.message.meta?.relation === 'reply' || attested.entry?.message.meta?.relation === 'followup'
        ? { relation: attested.entry.message.meta.relation } : {}),
    });
  }

  async list(auth: Auth, ref: ThreadRef, query: MessageQuery = {}) {
    return this.ctx.storage.transaction(async tx => {
      const thread = await getThread(tx, this.ctx, ref);
      if (!await canRead(tx, auth.actor, ref)) fail('forbidden', 'Read access is required.');
      const entries: Entry[] = [];
      for (const { value: entry } of await tx.scan<Entry>(key('entry', ref.owner, ref.id) + '/')) {
        if (!await canRead(tx, auth.actor, ref, entry)) continue;
        if (query.since_seq && decimal(entry.seq) <= decimal(query.since_seq)) continue;
        if (query.peer && entry.authenticated_author !== query.peer) continue;
        if (query.related_to && !same(entry.message.meta?.in_reply_to, query.related_to)) continue;
        if ((query.attention ?? 'inbox') === 'inbox') {
          const rows = await tx.scan<Attention>(key('attention', ref.owner, ref.id) + '/');
          if (!rows.some(a => a.value.recipient === auth.actor && same(a.value.message_ref, { sender: entry.message.sender, id: entry.message.id }))) continue;
        }
        entries.push(entry);
      }
      entries.sort((a, b) => compareOrdinal(a.seq, b.seq));
      return { ...page(entries, query, { ref, actor: auth.actor, peer: query.peer, related_to: query.related_to, attention: query.attention ?? 'inbox', since_seq: query.since_seq }, e => e.seq, compareOrdinal), retention: thread.messages_until ?? null };
    });
  }

  /**
   * The owner-attested obligation list for the caller. A reader whose membership no
   * longer admits the body gets the tombstone: the entry is stripped before the row
   * is stored back and before it is returned, so a revoked reader never sees content.
   */
  async attention(auth: Auth, ref: ThreadRef, options: PageOptions = {}) {
    return this.ctx.storage.transaction(async tx => {
      await getThread(tx, this.ctx, ref);
      const allowed = await canRead(tx, auth.actor, ref);
      const rows = (await tx.scan<Attention>(key('attention', ref.owner, ref.id) + '/')).map(r => r.value).filter(a => a.recipient === auth.actor && !a.acked);
      const visible: Attention[] = [];
      for (const row of rows) {
        if (row.disposition === 'expired') { visible.push(row); continue; }
        if (!allowed || row.entry === undefined || !await canRead(tx, auth.actor, ref, row.entry)) {
          const stripped: Attention = { ...row, entry: undefined, disposition: 'expired' };
          await tx.put(key('attention', ref.owner, ref.id, row.obligation_id), stripped);
          visible.push(stripped);
          continue;
        }
        visible.push(row);
      }
      visible.sort((a, b) => compareOrdinal(a.seq, b.seq));
      return page(visible, options, { ref, actor: auth.actor, kind: 'attention' }, a => a.seq, compareOrdinal);
    });
  }

  async ack(auth: Auth, ref: ThreadRef, id: string, mutation: Mutation) {
    return this.ctx.mutate(auth, `attention-ack/${canonical(ref)}/${id}`, mutation, {}, async tx => {
      await getThread(tx, this.ctx, ref);
      const slot = key('attention', ref.owner, ref.id, id);
      const attention = requireValue(await tx.get<Attention>(slot));
      if (attention.recipient !== auth.actor) fail('not_found', 'This attention obligation is not addressed to the caller.');
      attention.acked = true;
      await tx.put(slot, attention);
      const local = await tx.get<LocalAttention>(key('inbox', id));
      if (local) { local.attention = attention; await tx.put(key('inbox', id), local); }
      return { acked: true };
    });
  }

  async inbox(auth: Auth, options: PageOptions = {}) {
    if (auth.actor !== this.ctx.node) fail('forbidden', 'The local inbox belongs to this node.');
    return this.ctx.storage.transaction(async tx => {
      const rows = (await tx.scan<LocalAttention>('inbox/')).map(r => r.value);
      rows.sort((a, b) => a.attention.obligation_id.localeCompare(b.attention.obligation_id));
      const indexed = rows.map((value, index) => ({ value, position: String(index + 1) }));
      const result = page(indexed, options, { actor: auth.actor, kind: 'local-inbox' }, r => r.position, compareOrdinal);
      return { items: result.items.map(r => r.value), next_cursor: result.next_cursor };
    });
  }

  async getByRef(auth: Auth, ref: MessageRef) {
    return this.ctx.storage.transaction(async tx => {
      const entry = requireValue(await tx.get<Entry>(key('entry-ref', ref.sender, ref.id)));
      if (!await canRead(tx, auth.actor, entry.thread_ref, entry)) fail('not_found', 'This message is not readable.');
      return entry;
    });
  }

  async events(auth: Auth, ref: ThreadRef, options: PageOptions = {}) {
    return this.ctx.storage.transaction(async tx => {
      await getThread(tx, this.ctx, ref);
      if (!await canRead(tx, auth.actor, ref)) fail('forbidden', 'Read access is required.');
      const rows: Event[] = [];
      for (const { value: event } of await tx.scan<Event>(key('event', ref.owner, ref.id) + '/')) {
        if (event.message_ref) {
          const entry = await tx.get<Entry>(key('entry-ref', event.message_ref.sender, event.message_ref.id));
          if (!entry || !await canRead(tx, auth.actor, ref, entry)) continue;
        } else if (event.type === 'membership' && auth.actor !== this.ctx.node) continue;
        rows.push(event);
      }
      rows.sort((a, b) => compareOrdinal(a.id, b.id));
      return page(rows, options, { ref, actor: auth.actor, kind: 'events' }, e => e.id, compareOrdinal);
    });
  }

  async snapshot(auth: Auth, ref: ThreadRef) {
    return this.ctx.storage.transaction(async tx => {
      const thread = await getThread(tx, this.ctx, ref);
      if (!await canRead(tx, auth.actor, ref)) fail('forbidden', 'Read access is required.');
      const entries: Entry[] = [];
      for (const row of await tx.scan<Entry>(key('entry', ref.owner, ref.id) + '/')) if (await canRead(tx, auth.actor, ref, row.value)) entries.push(row.value);
      entries.sort((a, b) => compareOrdinal(a.seq, b.seq));
      return { thread, entries: entries.slice(0, 50), resume_cursor: encodeURIComponent(canonical({ scope: { ref, actor: auth.actor, kind: 'events' }, after: thread.event_seq })) };
    });
  }

  async expire(auth: Auth, ref: ThreadRef, beforeSeq: string) {
    this.ctx.owner(ref.owner);
    if (auth.actor !== this.ctx.node) fail('forbidden', 'Only the owner may expire history.');
    return this.ctx.storage.transaction(async tx => {
      await getThread(tx, this.ctx, ref);
      for (const row of await tx.scan<Entry>(key('entry', ref.owner, ref.id) + '/')) {
        if (decimal(row.value.seq) >= decimal(beforeSeq)) continue;
        const recordKey = key('message', row.value.message.sender, row.value.message.id);
        const record = requireValue(await tx.get<Remembered>(recordKey));
        if (Date.parse(record.retry_until) > this.ctx.now()) fail('conflict', 'The replay window is still open.');
        // The tombstone keeps the digest and the window, never the body.
        await tx.put(recordKey, { digest: record.digest, retry_until: record.retry_until });
        await tx.delete(row.key);
        await tx.delete(key('entry-ref', row.value.message.sender, row.value.message.id));
        const identity = { sender: row.value.message.sender, id: row.value.message.id };
        for (const attention of await tx.scan<Attention>(key('attention', ref.owner, ref.id) + '/')) {
          if (!same(attention.value.message_ref, identity)) continue;
          if (attention.value.disposition === 'expired') continue;
          const stripped: Attention = { ...attention.value, entry: undefined, disposition: 'expired' };
          await tx.put(attention.key, stripped);
          const local = await tx.get<LocalAttention>(key('inbox', attention.value.obligation_id));
          if (local) { local.attention = stripped; await tx.put(key('inbox', attention.value.obligation_id), local); }
        }

      }
      return { expired_before: beforeSeq };
    });
  }
}
