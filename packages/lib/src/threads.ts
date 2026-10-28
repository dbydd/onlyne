import { admissionView, decimal, fail, key, page, requireValue, utc } from './core.ts';
import type { Context } from './core.ts';
import type { Attention, Auth, Entry, Event, Grant, JoinedThread, Json, LocalAttention, Member, MessageRef, Mutation, Outbound, Page, PageOptions, ReadScope, ReplyPermit, Thread, ThreadDescription, ThreadRef, Transaction } from './types.ts';

/** One reserved default-thread id per owner: omitted targets land here, and the id is never reused. */
export const DEFAULT_THREAD = 'thr-default';

const DEFAULT_ACCESS: Member['access'] = ['post', 'read'];

function requireThreadRef(ref: ThreadRef): void {
  if (ref == null || typeof ref !== 'object' || typeof ref.owner !== 'string' || typeof ref.id !== 'string' || ref.id.length === 0) {
    fail('invalid_request', 'Expected a ThreadRef with an owner and a non-empty id.');
  }
}

async function loadThread(tx: Transaction, ctx: Context, ref: ThreadRef): Promise<Thread> {
  requireThreadRef(ref);
  ctx.owner(ref.owner);
  return requireValue(await tx.get<Thread>(key('thread', ref.owner, ref.id)));
}

function requireAccess(access: Array<'read' | 'post'>): Member['access'] {
  if (!Array.isArray(access) || access.length === 0) fail('invalid_request', 'access must list at least one of post and read.');
  for (const item of access) if (item !== 'post' && item !== 'read') fail('invalid_request', 'Unknown access member; expected post or read.');
  return [...new Set(access)];
}

function requireScope(scope: ReadScope): ReadScope {
  if (scope == null || typeof scope !== 'object' || Array.isArray(scope)) fail('invalid_request', 'read_scope must be an object.');
  for (const name of Object.keys(scope)) {
    if (name !== 'peers' && name !== 'since_seq' && name !== 'related_to') fail('invalid_request', `Unknown read_scope key ${name}.`);
  }
  const { peers, since_seq, related_to } = scope;
  if (peers !== undefined && (!Array.isArray(peers) || peers.length === 0 || peers.some(peer => typeof peer !== 'string' || peer.length === 0))) {
    fail('invalid_request', 'read_scope.peers must be a non-empty array of node ids.');
  }
  if (since_seq !== undefined && (typeof since_seq !== 'string' || !/^(0|[1-9][0-9]*)$/.test(since_seq))) {
    fail('invalid_request', 'read_scope.since_seq must be a decimal sequence string.');
  }
  if (related_to !== undefined && (typeof related_to?.sender !== 'string' || !related_to.sender || typeof related_to.id !== 'string' || !related_to.id)) {
    fail('invalid_request', 'read_scope.related_to must be a complete MessageRef.');
  }
  return { ...(peers === undefined ? {} : { peers: [...new Set(peers)] }), ...(since_seq === undefined ? {} : { since_seq }), ...(related_to === undefined ? {} : { related_to: { sender: related_to.sender, id: related_to.id } }) };
}

function inScope(entry: Entry, scope: ReadScope): boolean {
  if (scope.peers !== undefined && !scope.peers.includes(entry.authenticated_author)) return false;
  if (scope.since_seq !== undefined && BigInt(entry.seq) < BigInt(scope.since_seq)) return false;
  if (scope.related_to !== undefined) {
    const related = entry.message.meta?.in_reply_to;
    if (!related || related.sender !== scope.related_to.sender || related.id !== scope.related_to.id) return false;
  }
  return true;
}

function freshThread(ref: ThreadRef, title: string, at: string): Thread {
  return { thread_ref: ref, title, state: 'open', next_seq: '1', event_seq: '0', created_at: at, closed_at: null };
}

async function recordEvent(tx: Transaction, thread: Thread, type: Event['type'], data: Json): Promise<Thread> {
  const seq = (BigInt(thread.event_seq) + 1n).toString();
  const next: Thread = { ...thread, event_seq: seq };
  await tx.put(key('thread', thread.thread_ref.owner, thread.thread_ref.id), next);
  const event: Event = { id: seq, type, thread_ref: thread.thread_ref, data };
  await tx.put(key('event', thread.thread_ref.owner, thread.thread_ref.id, seq), event);
  return next;
}

/** Ascending by a unique string position, which is what core.page resumes on. */
function sortedBy<T>(items: T[], position: (item: T) => string): T[] {
  return items.sort((left, right) => {
    const a = position(left);
    const b = position(right);
    return a < b ? -1 : a > b ? 1 : 0;
  });
}

/** Reads a thread record under the caller's authority over its owner. */
export async function getThread(tx: Transaction, ctx: Context, ref: ThreadRef): Promise<Thread> {
  return loadThread(tx, ctx, ref);
}

/**
 * Read authority for a thread: the owner reads everything; any other actor reads
 * only as an active member holding read whose current read_scope admits the entry.
 * The scope is evaluated at read time, never stored on the entry.
 */
export async function canRead(tx: Transaction, actor: string, ref: ThreadRef, entry?: Entry): Promise<boolean> {
  const thread = await tx.get<Thread>(key('thread', ref.owner, ref.id));
  if (actor === thread?.thread_ref.owner) return true;
  if (entry !== undefined && (entry.thread_ref.owner !== ref.owner || entry.thread_ref.id !== ref.id)) return false;
  const member = await tx.get<Member>(key('member', ref.owner, ref.id, actor));
  if (member?.state !== 'active' || !member.access.includes('read')) return false;
  return entry === undefined || inScope(entry, member.read_scope);
}

/**
 * Issues the persistent grant notice an owner keeps until the recipient confirms it.
 * The notice id is the member revision, so one revision maps to one notice and a
 * retried membership change cannot queue a second delivery.
 */
async function issueGrant(tx: Transaction, ref: ThreadRef, peer: string, revision: string): Promise<Grant> {
  const grant: Grant = { id: revision, recipient: peer, thread_ref: ref, revision, acked: false };
  await tx.put(key('notice', grant.id), grant);
  return grant;
}

/** What a recipient keeps once an owner grants access, independent of the notice ack. */
interface Known {
  owner: string;
  thread_ref: ThreadRef;
  revision: string;
}

export class ThreadService {
  readonly #ctx: Context;

  constructor(ctx: Context) {
    this.#ctx = ctx;
  }

  async create(auth: Auth, mutation: Mutation & { title: string; id?: string; default?: boolean }): Promise<Thread> {
    if (!auth.actor) fail('unauthenticated', 'Authentication is required.');
    const owner = auth.actor;
    this.#ctx.owner(owner);
    if (typeof mutation.title !== 'string' || mutation.title.length === 0) fail('invalid_request', 'A thread needs a title.');
    if (mutation.id !== undefined && (typeof mutation.id !== 'string' || mutation.id.length === 0)) fail('invalid_request', 'Thread id must be a non-empty string.');
    const wantsDefault = mutation.default === true || mutation.id === DEFAULT_THREAD;
    const body = { title: mutation.title, id: mutation.id ?? null, default: wantsDefault };
    return this.#ctx.mutate(auth, 'thread.create', mutation, body, async tx => {
      const at = new Date(this.#ctx.now()).toISOString();
      const existingDefault = await tx.get<Thread>(key('thread', owner, DEFAULT_THREAD));
      if (wantsDefault) {
        if (existingDefault) {
          if (existingDefault.title === mutation.title || mutation.id === undefined) return existingDefault;
          const retitled = { ...existingDefault, title: mutation.title };
          await tx.put(key('thread', owner, DEFAULT_THREAD), retitled);
          return retitled;
        }
        const thread = freshThread({ owner, id: DEFAULT_THREAD }, mutation.title, at);
        await tx.put(key('thread', owner, DEFAULT_THREAD), thread);
        await tx.put(key('default', owner), thread.thread_ref);
        return thread;
      }
      let ref: ThreadRef;
      if (mutation.id === undefined) {
        ref = { owner, id: crypto.randomUUID() };
        while (await tx.get(key('thread', owner, ref.id))) ref = { owner, id: crypto.randomUUID() };
      } else {
        ref = { owner, id: mutation.id };
        if (await tx.get(key('thread', owner, ref.id)) || ref.id === DEFAULT_THREAD) fail('id_conflict', 'This thread id is already allocated for the owner.');
      }
      const thread = freshThread(ref, mutation.title, at);
      await tx.put(key('thread', owner, ref.id), thread);
      // An ordinary thread does not plant the landing slot: a default exists only when the
      // owner asks for one, so opening a conversation never turns later first contacts into
      // `not_a_member` refusals against a thread nobody joined.
      return thread;
    });
  }

  async close(auth: Auth, ref: ThreadRef, mutation: Mutation): Promise<Thread> {
    if (!auth.actor) fail('unauthenticated', 'Authentication is required.');
    requireThreadRef(ref);
    this.#ctx.owner(ref.owner);
    if (auth.actor !== ref.owner) fail('forbidden', 'Only the owner may close a thread.');
    const body = { ref };
    return this.#ctx.mutate(auth, `thread.close/${ref.owner}/${ref.id}`, mutation, body, async tx => {
      const thread = await loadThread(tx, this.#ctx, ref);
      if (thread.state === 'closed') fail('thread_closed', 'The thread is already closed.');
      return recordEvent(tx, { ...thread, state: 'closed', closed_at: new Date(this.#ctx.now()).toISOString() }, 'thread_closed', {});
    });
  }

  async list(auth: Auth, options: PageOptions = {}): Promise<Page<Thread>> {
    if (!auth.actor) fail('unauthenticated', 'Authentication is required.');
    this.#ctx.owner(auth.actor);
    return this.#ctx.storage.transaction(async tx => {
      const threads = sortedBy((await tx.scan<Thread>(key('thread', auth.actor) + '/')).map(row => row.value), thread => thread.thread_ref.id);
      return page(threads, options, { kind: 'threads', owner: auth.actor }, thread => thread.thread_ref.id);
    });
  }

  /**
   * Participant view: the threads this node was invited into and does not own, derived only
   * from the records it already holds — the known-owner rows and grant notices `acceptGrant`
   * wrote, the attention and inbox rows that name a thread, and the receipts of messages it
   * sent there. The title, the state and the member table belong to the owner, so they are
   * absent here; a caller that can reach the owner fills them in. This listing never invents
   * a member it could not read.
   */
  async joined(auth: Auth, options: PageOptions = {}): Promise<Page<JoinedThread>> {
    if (!auth.actor) fail('unauthenticated', 'Authentication is required.');
    if (auth.actor !== this.#ctx.node) fail('forbidden', 'The joined-thread view belongs to this node.');
    return this.#ctx.storage.transaction(async tx => {
      const rows = new Map<string, { ref: ThreadRef; seq: string; messages: Set<string> }>();
      const hold = (ref: ThreadRef, seq?: string, message?: MessageRef): void => {
        // A thread this node owns is already in `list()`; this view is the other side of it.
        if (ref.owner === this.#ctx.node) return;
        const slot = key(ref.owner, ref.id);
        const held = rows.get(slot) ?? { ref: { owner: ref.owner, id: ref.id }, seq: '0', messages: new Set<string>() };
        rows.set(slot, held);
        if (seq !== undefined && decimal(seq) > decimal(held.seq)) held.seq = seq;
        if (message !== undefined) held.messages.add(key(message.sender, message.id));
      };
      for (const row of await tx.scan<Known>(key('known') + '/')) hold(row.value.thread_ref);
      for (const row of await tx.scan<Grant>(key('grant') + '/')) hold(row.value.thread_ref);
      for (const row of await tx.scan<Attention>(key('attention') + '/')) hold(row.value.thread_ref, row.value.seq, row.value.message_ref);
      for (const row of await tx.scan<LocalAttention>(key('inbox') + '/')) hold(row.value.attention.thread_ref, row.value.attention.seq, row.value.attention.message_ref);
      for (const row of await tx.scan<Outbound>(key('outbound') + '/')) {
        const receipt = row.value.receipt;
        if (receipt !== undefined) hold(receipt.thread_ref, receipt.seq, receipt.message_ref);
      }
      const items = sortedBy([...rows.values()], row => key(row.ref.owner, row.ref.id))
        .map((row): JoinedThread => ({
          thread_ref: row.ref, last_seq: row.seq, message_count: row.messages.size, read_only: true,
        }));
      return page(items, options, { kind: 'joined', owner: auth.actor }, item => key(item.thread_ref.owner, item.thread_ref.id));
    });
  }

  async describe(auth: Auth, ref: ThreadRef): Promise<ThreadDescription> {
    if (!auth.actor) fail('unauthenticated', 'Authentication is required.');
    return this.#ctx.storage.transaction(async tx => {
      const thread = await loadThread(tx, this.#ctx, ref);
      if (!await canRead(tx, auth.actor, ref)) fail('forbidden', 'The caller does not hold read access to this thread.');
      const members = (await tx.scan<Member>(key('member', ref.owner, ref.id) + '/')).map(row => row.value);
      return { thread, members };
    });
  }

  async member(auth: Auth, ref: ThreadRef, peer: string, patch: Partial<Member>, mutation: Mutation): Promise<Member> {
    if (!auth.actor) fail('unauthenticated', 'Authentication is required.');
    requireThreadRef(ref);
    if (typeof peer !== 'string' || peer.length === 0) fail('invalid_request', 'A membership names its peer.');
    const isOwner = auth.actor === ref.owner;
    if (!isOwner && auth.actor !== peer) fail('forbidden', 'Only the owner manages other peers.');
    if (patch.revision !== undefined) fail('invalid_request', 'The revision is issued by the owner.');
    if (!isOwner && (patch.access !== undefined || patch.read_scope !== undefined || (patch.state !== undefined && patch.state !== 'revoked' && patch.state !== 'active'))) {
      fail('forbidden', 'A peer may only join or withdraw itself.');
    }
    const body = { ref, peer, patch };
    return this.#ctx.mutate(auth, `thread.member/${ref.owner}/${ref.id}/${peer}`, mutation, body, async tx => {
      const thread = await loadThread(tx, this.#ctx, ref);
      const current = await tx.get<Member>(key('member', ref.owner, ref.id, peer));
      let next: Member;
      if (!isOwner) {
        if (current === undefined) {
          if (patch.state === 'revoked') fail('not_found', 'No membership record to withdraw.');
          if (thread.state === 'closed') fail('thread_closed', 'A closed thread does not accept joins.');
          const admitted = (await this.#ctx.options.plugins.mayAdmit?.(auth.actor, ref, admissionView(tx))) === true;
          next = { peer, state: admitted ? 'active' : 'requested', access: [...DEFAULT_ACCESS], read_scope: {}, revision: this.#ctx.id() };
        } else if (patch.state === 'revoked') {
          if (current.state !== 'active' && current.state !== 'requested') fail('conflict', 'This membership is already stopped.');
          next = { ...current, state: 'revoked', access: [], revision: this.#ctx.id() };
        } else {
          fail('conflict', 'This peer already holds a membership record; only the owner changes it.');
        }
      } else {
        const state = patch.state ?? 'active';
        if (state !== 'requested' && state !== 'active' && state !== 'revoked' && state !== 'denied') fail('invalid_request', 'Unknown member state.');
        const stopping = state === 'revoked' || state === 'denied';
        const access = stopping ? [] : patch.access === undefined ? (current?.state === 'active' ? current.access : [...DEFAULT_ACCESS]) : requireAccess(patch.access);
        next = { peer, state, access: [...access], read_scope: patch.read_scope === undefined ? (current?.read_scope ?? {}) : requireScope(patch.read_scope), revision: this.#ctx.id() };
      }
      await tx.put(key('member', ref.owner, ref.id, peer), next);
      if (next.state === 'active' && current?.state !== 'active' && peer !== ref.owner) await issueGrant(tx, ref, peer, next.revision);
      await recordEvent(tx, thread, 'membership', JSON.parse(JSON.stringify(next)) as Json);
      return next;
    });
  }

  async members(auth: Auth, ref: ThreadRef, options: PageOptions = {}): Promise<Page<Member>> {
    if (!auth.actor) fail('unauthenticated', 'Authentication is required.');
    return this.#ctx.storage.transaction(async tx => {
      await loadThread(tx, this.#ctx, ref);
      if (!await canRead(tx, auth.actor, ref)) fail('forbidden', 'The caller does not hold read access to this thread.');
      const members = sortedBy((await tx.scan<Member>(key('member', ref.owner, ref.id) + '/')).map(row => row.value), member => member.peer);
      return page(members, options, { kind: 'members', ref }, member => member.peer);
    });
  }

  async defaultThread(auth: Auth): Promise<ThreadRef> {
    if (!auth.actor) fail('unauthenticated', 'Authentication is required.');
    this.#ctx.owner(auth.actor);
    return this.#ctx.storage.transaction(async tx => requireValue(await tx.get<ThreadRef>(key('default', auth.actor))));
  }

  /**
   * Owner-side notice outbox: every grant notice this node issued that has no ack yet.
   * Only the owner calling as ctx.node may read this list; each item carries recipient
   * so the client knows where to POST the full notice.
   */
  async notices(auth: Auth, options: PageOptions = {}): Promise<Page<Grant>> {
    if (!auth.actor) fail('unauthenticated', 'Authentication is required.');
    this.#ctx.owner(auth.actor);
    return this.#ctx.storage.transaction(async tx => {
      const held = sortedBy((await tx.scan<Grant>(key('notice') + '/')).map(row => row.value).filter(grant => !grant.acked), grant => grant.id);
      return page(held, options, { kind: 'notices', owner: auth.actor }, grant => grant.id);
    });
  }

  /**
   * Recipient view: the grants this node accepted from owners, until it confirms each one.
   * A missed push is fetched over HTTP and processed with acceptGrant; this list is what
   * the confirmation pass walks to close the owner's notice.
   */
  async grants(auth: Auth, options: PageOptions = {}): Promise<Page<Grant>> {
    if (!auth.actor) fail('unauthenticated', 'Authentication is required.');
    return this.#ctx.storage.transaction(async tx => {
      const held = sortedBy((await tx.scan<Grant>(key('grant') + '/')).map(row => row.value).filter(grant => !grant.acked), grant => grant.id);
      return page(held, options, { kind: 'grants', recipient: auth.actor }, grant => grant.id);
    });
  }

  /** Recipient side: records that the owner confirmed this grant, so it is not confirmed twice. */
  async ackGrantLocally(auth: Auth, ref: ThreadRef, id: string): Promise<void> {
    if (!auth.actor) fail('unauthenticated', 'Authentication is required.');
    await this.#ctx.storage.transaction(async tx => {
      const slot = key('grant', ref.owner, id);
      const stored = await tx.get<Grant>(slot);
      if (stored !== undefined && !stored.acked) await tx.put(slot, { ...stored, acked: true });
    });
  }

  /**
   * Recipient side of grant acceptance: an owner pushes us a notice we received during
   * a prior join. We store both the notice and the known-owner record keyed owner+threadid.
   * Caller is the owner the notice names, notice is addressed to our node, thread stays on owner.
   *
   * A notice that names `previous_thread_ref` is a re-issue after its owner changed identity:
   * the ref it names is the one this node learned before, so that known row is dropped. Two
   * rows for one thread would make every reconciliation pass pull the dead owner.
   */
  async acceptGrant(auth: Auth, grant: Grant): Promise<Grant> {
    if (!auth.actor) fail('unauthenticated', 'Authentication is required.');
    if (grant == null || typeof grant !== 'object' || typeof grant.id !== 'string' || grant.id.length === 0 || typeof grant.revision !== 'string' || grant.revision.length === 0) {
      fail('invalid_request', 'Expected a grant notice with an id and a revision.');
    }
    requireThreadRef(grant.thread_ref);
    if (grant.thread_ref.owner !== auth.actor) fail('forbidden', 'A grant notice must come from the thread owner it names.');
    if (grant.recipient !== this.#ctx.node) fail('forbidden', 'The grant notice is addressed to another node.');
    const previous = grant.previous_thread_ref;
    if (previous !== undefined) requireThreadRef(previous);
    const ref: ThreadRef = { owner: grant.thread_ref.owner, id: grant.thread_ref.id };
    return this.#ctx.storage.transaction(async tx => {
      const slot = key('grant', ref.owner, grant.id);
      const stored = await tx.get<Grant>(slot);
      if (stored !== undefined && (stored.recipient !== this.#ctx.node || stored.revision !== grant.revision || stored.thread_ref.id !== ref.id)) {
        fail('id_conflict', 'The grant id is bound to another notice.');
      }
      const known: Known = { owner: ref.owner, thread_ref: ref, revision: grant.revision };
      await tx.put(key('known', ref.owner, ref.id), known);
      if (previous !== undefined && (previous.owner !== ref.owner || previous.id !== ref.id)) {
        await tx.delete(key('known', previous.owner, previous.id));
      }
      if (stored !== undefined) return stored;
      const next: Grant = { id: grant.id, recipient: this.#ctx.node, thread_ref: ref, revision: grant.revision, acked: false };
      await tx.put(slot, next);
      return next;
    });
  }

  /**
   * Acknowledge a notice on the owner node: owner receives our confirmation, updates
   * the owner-issued notice to acked:true, stops retries. Caller must be recipient of the notice.
   */
  async ackGrant(auth: Auth, id: string, mutation: Mutation): Promise<Grant> {
    if (!auth.actor) fail('unauthenticated', 'Authentication is required.');
    if (typeof id !== 'string' || id.length === 0) fail('invalid_request', 'A grant id is required.');
    const body = { id };
    return this.#ctx.mutate(auth, `thread.grant.ack/${id}`, mutation, body, async tx => {
      const grant = requireValue(await tx.get<Grant>(key('notice', id)));
      if (grant.recipient !== auth.actor) fail('forbidden', 'The grant notice is addressed to another recipient.');
      if (grant.acked) return grant;
      const acked: Grant = { ...grant, acked: true };
      await tx.put(key('notice', id), acked);
      return acked;
    });
  }

  async permits(auth: Auth, permit: ReplyPermit, mutation: Mutation): Promise<ReplyPermit> {
    if (!auth.actor) fail('unauthenticated', 'Authentication is required.');
    const original: MessageRef | undefined = permit?.message_ref;
    if (typeof original?.sender !== 'string' || !original.sender || typeof original.id !== 'string' || !original.id) fail('invalid_request', 'A permit binds a complete MessageRef.');
    if (original.sender !== auth.actor) fail('forbidden', 'Only the original message sender may issue a reply permit.');
    if (typeof permit.grantee !== 'string' || !permit.grantee || permit.grantee === auth.actor) fail('invalid_request', 'A permit names one grantee other than its issuer.');
    requireThreadRef(permit.landing);
    this.#ctx.owner(permit.landing.owner);
    if (!Number.isInteger(permit.remaining) || permit.remaining < 1) fail('invalid_request', 'A permit needs a bounded positive reply count.');
    if (utc(permit.expires_at) <= this.#ctx.now()) fail('invalid_request', 'A permit needs a future expiry.');
    const body = { message_ref: { sender: original.sender, id: original.id }, grantee: permit.grantee, landing: permit.landing, expires_at: permit.expires_at, remaining: permit.remaining, followup: permit.followup === true };
    return this.#ctx.mutate(auth, `thread.permit/${original.sender}/${original.id}/${permit.grantee}`, mutation, body, async tx => {
      const thread = await loadThread(tx, this.#ctx, permit.landing);
      if (thread.state === 'closed') fail('thread_closed', 'A permit cannot land on a closed thread.');
      const stored: ReplyPermit = { ...body, revoked: permit.revoked === true };
      await tx.put(key('permit', original.sender, original.id, permit.grantee), stored);
      return stored;
    });
  }
}
