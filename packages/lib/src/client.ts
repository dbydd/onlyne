import type { Context } from './core.js';
import { canonical, decimal, fail, key, requireValue, utc } from './core.js';
import type {
  Action, Attention, Auth, DeliveryPlan, DomainMembership, DomainMutation, Entry, ErrorCode, Failure, Json, LocalAttention,
  InjectOutcome, Message, MessageQuery, MsgInput, Mutation, Outbound, Page, PageOptions, SendRequest, TraceEvent,
  Thread, ThreadRef, Transaction, Transport,
} from './types.js';
import type { MessageService } from './messages.js';
import type { ResourceService } from './resources.js';
import type { ThreadService } from './threads.js';
import type { DomainService } from './domains.js';

/** Result of one delivery: `admission` is what the peer recorded, `wait` is what this caller observed. */
export interface SendOutcome {
  receipt?: Outbound['receipt'];
  admission: Outbound['state'];
  reply?: Entry;
  /**
   * Whether the reply body is safe to render. `withheld` means the same body is already
   * confirmed consumed in this context through another path (RFC 0008 §5), so the caller
   * renders the arrival facts and the original message reference only.
   */
  reply_body?: 'included' | 'withheld';
  wait?: 'answered' | 'timed_out' | 'cancelled';
  error?: Failure;
}

export interface SendOptions {
  signal?: AbortSignal;
  retry_until?: string;
  idempotency_key?: string;
  expected_author?: string;
  /**
   * The sender's own consumption identity for the awaited reply (RFC 0008 §5). It stays
   * local and never stands in for the wire plan's receiver-side context.
   */
  context?: string;
}

export interface MsgOptions {
  signal?: AbortSignal;
  /** The sender's own consumption identity for an awaited reply; `msg` passes it to `send`. */
  context?: string;
}

interface Waiter {
  original: Message;
  expected_author: string;
  /** The sender-local context this await consumes its reply in; absent for a generic caller. */
  local_context?: string;
  outcome: Promise<Entry>;
  settle(entry: Entry): void;
}

/** An outbound record plus the digest that fixes its message identity and the sender's own consumption identity. */
interface StoredOutbound extends Outbound { digest?: string; local_context?: string }

/** Codes the peer records as a terminal refusal before acceptance (RFC 0003 §5). */
const TERMINAL: Record<string, true> = {
  invalid_request: true, forbidden: true, not_a_member: true, not_found: true,
  id_conflict: true, revision_conflict: true, conflict: true, thread_closed: true,
  retry_window_expired: true, payload_too_large: true, unsupported_media_type: true,
  unsupported_delivery: true, ambiguous_target: true, ambiguous_resource: true,
};
const MAX_CURSOR_PAGES = 10_000;

/** Anything that is not a recorded refusal leaves the admission unknown, never "undelivered". */
function admissionOf(failure: Failure): Outbound['state'] {
  return failure.retryable === false && TERMINAL[failure.code] === true ? 'refused' : 'unknown';
}

function failureOf(error: unknown): Failure {
  const reported = (error as { error?: Partial<Failure> }).error;
  if (reported !== undefined && typeof reported.code === 'string' && typeof reported.message === 'string') {
    return reported as Failure;
  }
  return {
    code: 'unavailable',
    message: error instanceof Error ? error.message : 'The transport failed without a response.',
    retryable: true,
  };
}

/**
 * Request identity for one delivery (RFC 0003 §3 step 3). The plan, the local
 * idempotency key and `retry_until` describe delivery, not the message.
 */
function messageDigest(request: SendRequest): string {
  const meta = request.message.meta;
  return canonical({
    sender: request.message.sender,
    target_endpoint: request.target_endpoint,
    thread_ref: meta?.thread ?? null,
    in_reply_to: meta?.in_reply_to ?? null,
    relation: meta?.relation ?? null,
    mentions: meta?.mentions ?? null,
    delivery_kind: request.message.delivery_kind,
    msg: request.message.msg,
    parts: meta?.parts ?? null,
    data: meta?.data ?? null,
  });
}

/** A legitimate reply to `original`: same reference, reply relation, a different authenticated author. */
function isReply(original: Pick<Message, 'sender' | 'id'>, entry: Entry): boolean {
  const meta = entry.message.meta;
  const related = meta?.in_reply_to;
  if (related === undefined || related.sender !== original.sender || related.id !== original.id) return false;
  if (meta?.relation !== undefined && meta.relation !== 'reply') return false;
  return entry.authenticated_author !== original.sender;
}

/** Replay order is the thread's own: `(owner, id)` then `seq` as a number, never a UUID sort. */
function replayOrder(a: Entry | undefined, b: Entry | undefined): number {
  if (a === undefined) return b === undefined ? 0 : -1;
  if (b === undefined) return 1;
  if (a.thread_ref.owner !== b.thread_ref.owner) return a.thread_ref.owner < b.thread_ref.owner ? -1 : 1;
  if (a.thread_ref.id !== b.thread_ref.id) return a.thread_ref.id < b.thread_ref.id ? -1 : 1;
  const left = decimal(a.seq);
  const right = decimal(b.seq);
  return left < right ? -1 : left > right ? 1 : 0;
}

const outboundKey = (sender: string, id: string) => key('outbound', sender, id);
const inboxKey = (obligation_id: string) => key('inbox', obligation_id);
const replyKey = (original: Pick<Message, 'sender' | 'id'>) => key('reply', original.sender, original.id);
/** One consumption slot per `(context, ThreadRef, seq)`, shared by every delivery path (RFC 0004 §5). */
const consumptionKey = (context: string, thread_ref: ThreadRef, seq: string) => key('consumed', context, thread_ref.owner, thread_ref.id, seq);
/** The todo one consumed entry owes the host, until its hook succeeds. */
const todoKey = (context: string, thread_ref: ThreadRef, seq: string) => key('todo', context, thread_ref.owner, thread_ref.id, seq);
/** In-process identity of one thread entry: its `ThreadRef` and sequence. */
const entryKey = (thread_ref: ThreadRef, seq: string) => key(thread_ref.owner, thread_ref.id, seq);

/** Confirmed consumption for one slot, written by whichever path the host confirmed. */
interface ConsumptionRecord {
  context: string; thread_ref: ThreadRef; seq: string;
  confirmed: boolean; via: 'tool_result' | 'fragment'; at: string;
}

/** A todo the host still owes for one consumed entry; `done` is written after its hook succeeds. */
interface PendingTodo { entry: Entry; context: string; done: boolean }

/** The durable part of an obligation: identity and owner-attested facts, body optional. */
function attentionOf(entry: Entry, obligation_id: string, recipient: string): Attention {
  return {
    obligation_id, recipient, thread_ref: entry.thread_ref, seq: entry.seq,
    message_ref: { sender: entry.message.sender, id: entry.message.id },
    authenticated_author: entry.authenticated_author, acked: false, entry,
  };
}

export class OnlyneClient {
  readonly #ctx: Context;
  readonly #transport: Transport;
  readonly #messages: MessageService;
  readonly #threads: ThreadService;
  readonly #resources: ResourceService;
  readonly #domains: DomainService;
  /** Live waiters, keyed by the original message; an entry lives only while its `send` runs. */
  readonly #waiters = new Map<string, Waiter>();
  /** Replies already returned as tool results, per context, held until consumption confirms them. */
  readonly #held = new Map<string, Set<string>>();

  constructor(
    ctx: Context, transport: Transport, messages: MessageService, threads: ThreadService,
    resources: ResourceService, domains: DomainService,
  ) {
    this.#ctx = ctx;
    this.#transport = transport;
    this.#messages = messages;
    this.#threads = threads;
    this.#resources = resources;
    this.#domains = domains;
  }

  // ---- delivery ---------------------------------------------------------------

  /**
   * Delivers one message under a caller-supplied plan. The plan is never re-derived
   * here: RFC 0005 §2 forbids the library from choosing a delivery mode itself.
   */
  async send(to: string, message: Message, plan: DeliveryPlan, options: SendOptions = {}): Promise<SendOutcome> {
    const ctx = this.#ctx;
    if (message.sender !== ctx.options.node_id) {
      fail('forbidden', 'This node may only deliver messages this node authors.');
    }
    if (typeof to !== 'string' || to.length === 0) fail('invalid_request', 'A delivery names its target endpoint.');
    if (plan.response === 'await') await this.#ensureAwaitable(plan, options.signal);
    // RFC 0005 §3: an await that only this node could answer is a cycle, not a wait.
    if (plan.response === 'await' && (to === ctx.options.node_id || options.expected_author === ctx.options.node_id)) {
      fail('unsupported_delivery', 'This node cannot await a reply only it could author; no message was sent.');
    }
    // This node owns its own threads, so appending there is a local admit, not a delivery.
    if (to === ctx.options.node_id) return this.#admitHere(message, plan);

    const slot = outboundKey(message.sender, message.id);
    const request: SendRequest = {
      idempotency_key: options.idempotency_key ?? ctx.id(),
      retry_until: this.#window(options.retry_until),
      message,
      target_endpoint: to,
      plan,
    };
    const digest = messageDigest(request);
    // The same message identity with different content is a conflict, never a rewrite:
    // the slot keeps the first request and the caller sends a new message id instead.
    const record = await ctx.storage.transaction(async tx => {
      const stored = await tx.get<StoredOutbound>(slot);
      if (stored !== undefined) {
        if ((stored.digest ?? messageDigest(stored.request)) !== digest) {
          fail('id_conflict', 'This message identity is already bound to different content.');
        }
        // A re-drive of the same message keeps the first request identity, which the peer
        // dedupes on; the consumption identity belongs to the caller waiting now, not to
        // the delivery, so a caller that names another one replaces the stale slot.
        if (options.context === undefined || stored.local_context === options.context) return stored;
        const updated: StoredOutbound = { ...stored, local_context: options.context };
        await tx.put(slot, updated);
        return updated;
      }
      const fresh: StoredOutbound = {
        request, expected_author: options.expected_author ?? to, state: 'pending', digest,
        ...(options.context === undefined ? {} : { local_context: options.context }),
      };
      await tx.put(slot, fresh);
      return fresh;
    });

    if (plan.response !== 'await') return this.#transmit(record, options.signal);

    const waiter = this.#register(record);
    try {
      return await this.#await(record, waiter, options.signal);
    } finally {
      if (this.#waiters.get(replyKey(message)) === waiter) this.#waiters.delete(replyKey(message));
    }
  }

  /**
   * Re-drives outbound records still open inside their window (RFC 0003 §3). `unknown`
   * is terminal in meaning but the window still owns another attempt, so the window
   * decides when to stop, not the last recorded state. A record whose window closed is
   * settled as `unknown`, never left looking pending.
   */
  async recover(signal?: AbortSignal): Promise<void> {
    const rows = await this.#ctx.storage.transaction(async tx =>
      (await tx.scan<StoredOutbound>(key('outbound') + '/'))
        .map(row => row.value)
        .filter(row => row.digest !== undefined && (row.state === 'pending' || row.state === 'unknown')));
    for (const row of rows) {
      if (signal?.aborted) return;
      if (utc(row.request.retry_until) <= this.#ctx.now()) {
        if (row.state !== 'unknown') {
          await this.#commit(row, {
            state: 'unknown',
            error: { code: 'retry_window_expired', message: 'The window closed before this node learned the admission.', retryable: false },
          });
          this.#trace({ event: 'outbound.expired', sender: row.request.message.sender, message: row.request.message.id, code: 'retry_window_expired' });
        }
        continue;
      }
      // A recovered record has no caller left to consume a reply, so recovery re-drives the
      // delivery and lets reconciliation surface any answer through the local inbox.
      this.#trace({ event: 'outbound.redriven', sender: row.request.message.sender, message: row.request.message.id, target: row.request.target_endpoint });
      await this.#transmit({ ...row, state: 'pending' }, signal);
    }
  }

  /** One structured line per lifecycle transition; the host decides where the line lands. */
  #trace(event: TraceEvent): void {
    try { this.#ctx.options.plugins.runtime?.trace?.(event); } catch { /* a tracing fault never breaks delivery */ }
  }

  #injectTrace(entry: Entry, record: LocalAttention, outcome: InjectOutcome, reason?: string, context?: string): void {
    this.#trace({
      event: 'inbound.inject', thread: entry.thread_ref, seq: entry.seq,
      context: context ?? record.context ?? this.#ctx.options.node_id, outcome,
      ...(reason === undefined ? {} : { reason }),
    });
  }

  /** Appends to a thread this node hosts: no outbound record, no peer, no network. */
  async #admitHere(message: Message, plan: DeliveryPlan): Promise<SendOutcome> {
    // A local append cannot await: the answer lands here as another local admit, which no
    // waiter on an outbound delivery observes. Refusing is the only honest answer.
    if (plan.response === 'await') {
      fail('unsupported_delivery', 'A local append cannot await its answer; no message was sent.');
    }
    const receipt = await this.#messages.admit(this.#auth(), {
      idempotency_key: this.#ctx.id(),
      retry_until: this.#window(undefined),
      message,
      target_endpoint: this.#ctx.options.node_id,
      plan,
    });
    return { admission: 'accepted', receipt };
  }

  // ---- inbound path -----------------------------------------------------------

  /**
   * Records one inbound attention item, then acts. The durable entry and the local
   * reply record commit before the ack (RFC 0003 §2); only then may a waiter be
   * released, and only by an entry whose author the owner authenticated and whose
   * author is the one expected. Invalid evidence is still acked but resolves nothing.
   *
   * The entry is trusted as-is: this is the runtime binding's own receive path, where
   * the owner's authenticated response is the evidence, not a caller argument. No
   * HTTP route accepts an `Entry` from a peer, so no peer can supply one.
   */
  async receive(entry: Entry, obligation_id: string, context?: string): Promise<LocalAttention> {
    const attention: Attention = attentionOf(entry, obligation_id, this.#ctx.options.node_id);
    const valid = this.#attest(attention, entry.thread_ref.owner);
    let duplicate = false;
    const queued: LocalAttention = valid
      ? await this.#ctx.storage.transaction(async tx => {
          const stored = await tx.get<LocalAttention>(inboxKey(obligation_id));
          if (stored !== undefined) {
            duplicate = true;
            return stored;
          }
          const record: LocalAttention = { attention, ...(context === undefined ? {} : { context }), injected: false };
          await tx.put(inboxKey(obligation_id), record);
          return record;
        })
      : { attention, ...(context === undefined ? {} : { context }), injected: false };

    const relation = entry.message.meta?.relation;
    this.#trace(duplicate
      ? { event: 'inbound.duplicate', sender: entry.message.sender, message: entry.message.id }
      : {
          event: 'inbound.received', obligation: obligation_id, thread: entry.thread_ref, seq: entry.seq,
          sender: entry.message.sender, valid, injected: queued.injected,
          ...(relation === 'reply' || relation === 'followup' ? { relation } : {}),
          ...(queued.context === undefined ? {} : { context: queued.context }),
        });

    // `receipt` and `followup` never release a waiter; only a reply to the original does.
    if (valid) this.#settle(entry);

    if (!queued.attention.acked && valid) await this.#ack(entry.thread_ref, obligation_id);
    if (valid) await this.#consume(entry, queued);
    return queued;
  }

  /**
   * Reconciles unacked attention for every owner this node knows (RFC 0003 §2): the
   * `owner + thread_ref` pairs recorded when a grant notice was accepted, plus the
   * threads this node hosts, which need no grant. The owner's authenticated attention
   * list is the evidence; the ack goes out only after the entry is durable locally.
   */
  async reconcile(signal?: AbortSignal): Promise<Page<LocalAttention>> {
    for (const ref of await this.#knownRefs()) {
      if (signal?.aborted) break;
      try {
        await this.#reconcileThread(ref, signal);
      } catch (error) {
        if (ref.owner === this.#ctx.options.node_id) throw error;
        // One owner this node cannot reach or resolve must not starve the rest; the next
        // reconciliation pass retries it, and the local inbox still reflects what landed.
        continue;
      }
    }
    return this.#messages.inbox(this.#auth(), {});
  }

  /**
   * Delivers outstanding owner grant notices, replays owed todos and entries whose durable
   * record never reached the model, then reconciles. Replay follows each thread's own order:
   * `owner`, `id`, then `seq` as a number. One record that fails never starves the rest, and
   * the failures are thrown after the pass with their records still owed for the next one.
   * Caller-scheduled.
   */
  async maintain(signal?: AbortSignal): Promise<void> {
    await this.#deliverGrants(signal);
    if (signal?.aborted) return;
    await this.#confirmGrants(signal);
    if (signal?.aborted) return;
    const failures: unknown[] = [];
    // Owed todos replay before the inbox pass, so a hook that fails now waits for the next
    // pass instead of being retried inside this one.
    await this.#replayTodos(signal, failures);
    if (signal?.aborted) return;
    await this.#replayInbox(signal, failures);
    if (signal?.aborted) return;
    try {
      await this.reconcile(signal);
    } catch (error) {
      failures.push(error);
    }
    if (failures.length === 1) throw failures[0];
    if (failures.length > 1) throw new AggregateError(failures, 'Some owed records could not be settled; the rest were.');
  }

  /**
   * Whether any durable record still needs a local pass: an unacked grant notice, an
   * owed todo, an uninjected inbox row, or an outbound stuck pending/unknown. A scheduler
   * consults this before running `maintain`, so an idle node costs no rounds; anything
   * that flips this on (an admission, an ack, a failed delivery) also wakes the host.
   */
  async hasPendingWork(): Promise<boolean> {
    return this.#ctx.storage.transaction(async tx => {
      const inbox = await tx.scan<LocalAttention>(key('inbox') + '/');
      if (inbox.some(row => !row.value.attention.acked || !row.value.injected && row.value.attention.entry !== undefined && row.value.attention.disposition !== 'expired')) return true;
      const todos = await tx.scan<PendingTodo>(key('todo') + '/');
      if (todos.some(row => !row.value.done)) return true;
      const outbound = await tx.scan<StoredOutbound>(key('outbound') + '/');
      if (outbound.some(row => row.value.digest !== undefined && (row.value.state === 'pending' || row.value.state === 'unknown'))) return true;
      return false;
    });
  }

  /** Replays todos that are owed but whose hook has not succeeded, in thread order. */
  async #replayTodos(signal: AbortSignal | undefined, failures: unknown[]): Promise<void> {
    const owed = await this.#ctx.storage.transaction(async tx =>
      (await tx.scan<PendingTodo>(key('todo') + '/')).map(row => row.value).filter(row => !row.done));
    owed.sort((a, b) => replayOrder(a.entry, b.entry));
    for (const row of owed) {
      if (signal?.aborted) return;
      try {
        await this.#deliverTodo(row.entry, row.context);
      } catch (error) {
        failures.push(error);
      }
    }
  }

  /**
   * Replays inbox records the model has not seen. A record already delivered is skipped, but
   * an entry durable here still answers a live await, and its owed todo replays above even
   * when the record is already injected.
   */
  async #replayInbox(signal: AbortSignal | undefined, failures: unknown[]): Promise<void> {
    const queued = await this.#ctx.storage.transaction(async tx =>
      (await tx.scan<LocalAttention>(key('inbox') + '/')).map(row => row.value));
    queued.sort((a, b) => replayOrder(a.attention.entry, b.attention.entry));
    for (const record of queued) {
      if (signal?.aborted) return;
      const entry = record.attention.entry;
      if (entry === undefined) continue;
      // An entry already durable here answers a live await even when the runtime never saw it.
      this.#settle(entry);
      if (record.injected || record.attention.disposition === 'expired') continue;
      try {
        await this.#consume(entry, record);
      } catch (error) {
        failures.push(error);
      }
    }
  }

  // ---- the single agent entry --------------------------------------------------

  /**
   * The one call a model makes. SystemOne turns the intent into a typed action, the
   * decision is persisted with its input, then exactly that action runs. With no
   * usable decision the request fails: the library never picks a plan itself.
   */
  async msg(input: MsgInput, options: MsgOptions = {}): Promise<unknown> {
    if (input == null || typeof input.delivery_kind !== 'string' || typeof input.msg !== 'string') {
      fail('invalid_request', 'msg needs string delivery_kind and msg fields.');
    }
    const action = await this.#decide(input);
    if (action.kind !== 'send') return this.#dispatch(action, options);

    if (action.response === undefined || action.execution === undefined) {
      fail('decision_unavailable', 'A send needs response and execution; no plan was chosen.');
    }
    const meta = input.meta;
    const relation = action.in_reply_to === undefined ? action.relation : action.relation ?? 'reply';
    const message: Message = {
      id: this.#ctx.id(),
      sender: this.#ctx.options.node_id,
      delivery_kind: input.delivery_kind,
      msg: input.msg,
      ...(action.thread === undefined && action.in_reply_to === undefined && action.mentions === undefined && meta === undefined
        ? {}
        : {
            meta: {
              ...(action.thread === undefined ? {} : { thread: action.thread }),
              ...(action.in_reply_to === undefined ? {} : { in_reply_to: action.in_reply_to }),
              ...(relation === undefined ? {} : { relation }),
              ...(action.mentions === undefined ? {} : { mentions: action.mentions }),
              ...(meta === undefined ? {} : { data: meta }),
            },
          }),
    };
    const plan: DeliveryPlan = {
      response: action.response, execution: action.execution,
      ...(action.context === undefined ? {} : { context: action.context }),
    };
    // A thread this node hosts is appended locally only when the decision targets this node.
    // A send for another peer that happens to name a local thread is a contradiction: refusing
    // it is what keeps a delivery from being reported as accepted while nothing left the node.
    if (action.thread !== undefined && action.thread.owner === this.#ctx.options.node_id) {
      if (action.to !== this.#ctx.options.node_id) {
        fail('invalid_request', `The decision names local thread ${action.thread.id} but targets ${action.to}; a delivery cannot be a local append.`);
      }
      return this.#admitHere(message, plan);
    }
    const target = await this.#ctx.options.plugins.resolve(action.to);
    if (target.node_id !== action.to) {
      fail('unavailable', 'The resolver answered with another identity than the decision named.');
    }
    return this.send(target.node_id, message, plan, {
      ...(options.signal === undefined ? {} : { signal: options.signal }),
      ...(options.context === undefined ? {} : { context: options.context }),
    });
  }

  // ---- internals ---------------------------------------------------------------

  #auth(): Auth {
    return { actor: this.#ctx.options.node_id };
  }

  /** Persists the decision and its outcome. RFC 0005 §2: a decision the library cannot act on is a recorded failure. */
  async #decide(input: MsgInput): Promise<Action> {
    const decision_id = this.#ctx.id();
    const at = new Date(this.#ctx.now()).toISOString();
    let action: Action;
    try {
      action = await this.#ctx.options.plugins.decide(input);
    } catch (error) {
      const failure = failureOf(error);
      await this.#ctx.storage.transaction(tx => tx.put(key('decision', decision_id), { input, at, error: failure }));
      if (failure.code !== 'decision_unavailable' && failure.code !== 'unavailable') throw error;
      fail('decision_unavailable', failure.message);
    }
    if (action == null || typeof action !== 'object' || !['send', 'read', 'thread', 'resource', 'domain'].includes(action.kind)) {
      await this.#ctx.storage.transaction(tx => tx.put(key('decision', decision_id), {
        input, at,
        error: { code: 'decision_unavailable' as ErrorCode, message: 'The decision is not an executable action.', retryable: true },
      }));
      fail('decision_unavailable', 'The decision is not an executable action.');
    }
    await this.#ctx.storage.transaction(tx => tx.put(key('decision', decision_id), { input, action, at }));
    return action;
  }

  /**
   * Only `await` needs the host capability, and it is checked before sending (RFC 0005 §5).
   * `async` ends at the peer and never holds the call open, so it needs nothing here.
   */
  async #ensureAwaitable(plan: DeliveryPlan, signal: AbortSignal | undefined): Promise<void> {
    if (signal?.aborted) throw signal.reason ?? new Error('The send was cancelled.');
    const runtime = this.#ctx.options.plugins.runtime;
    if (runtime === undefined || !(await runtime.supports(plan))) {
      fail('unsupported_delivery', 'The host cannot hold this await, so no message was sent.');
    }
  }

  /** RFC 0003 §8: the declared window never exceeds the node's supported maximum. */
  #window(requested: string | undefined): string {
    const ceiling = new Date(this.#ctx.now() + (this.#ctx.options.retry_max_ms ?? 86_400_000)).toISOString();
    if (requested === undefined) return ceiling;
    return utc(requested) < utc(ceiling) ? requested : ceiling;
  }

  /** One attempt's budget: the wait bound, never past the admission window. */
  #attempt(record: Outbound): number {
    const wait = this.#ctx.options.wait_ms ?? 60_000;
    const remaining = utc(record.request.retry_until) - this.#ctx.now();
    return Math.max(1, Math.min(wait, remaining));
  }

  /**
   * A network call that cannot outlive its attempt: the caller's signal cancels it, and
   * even with no signal the attempt ends on its own deadline, so an unreachable peer or
   * a hanging resolver cannot pin a delivery forever (RFC 0005 §3).
   */
  async #remote<T>(
    target: string, method: string, path: string, body: unknown, signal: AbortSignal | undefined, budget: number,
  ): Promise<T> {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(new Error('The attempt exceeded its deadline.')), budget);
    const onAbort = () => controller.abort(signal?.reason);
    if (signal !== undefined) {
      if (signal.aborted) controller.abort(signal.reason);
      else signal.addEventListener('abort', onAbort, { once: true });
    }
    try {
      return await this.#transport.request<T>(target, method, path, body, controller.signal);
    } finally {
      clearTimeout(timer);
      signal?.removeEventListener('abort', onAbort);
    }
  }

  async #transmit(record: StoredOutbound, signal: AbortSignal | undefined): Promise<SendOutcome> {
    const request = record.request;
    try {
      const thread = request.message.meta?.thread;
      const path = thread === undefined
        ? '/messages'
        : `/threads/${encodeURIComponent(thread.owner)}/${encodeURIComponent(thread.id)}/messages`;
      const receipt = await this.#remote<NonNullable<Outbound['receipt']>>(
        request.target_endpoint, 'POST', path, request, signal, this.#attempt(record));
      await this.#commit(record, { state: 'accepted', receipt });
      const guarantee = (receipt as { guarantee?: unknown }).guarantee;
      this.#trace({
        event: 'outbound.admitted', sender: request.message.sender, message: request.message.id,
        target: request.target_endpoint, state: 'accepted', ...(typeof guarantee === 'string' ? { guarantee } : {}),
      });
      return { admission: 'accepted', receipt };
    } catch (error) {
      if (signal?.aborted) throw error;
      const failure = failureOf(error);
      await this.#commit(record, { state: admissionOf(failure), error: failure });
      this.#trace({
        event: 'outbound.admitted', sender: request.message.sender, message: request.message.id,
        target: request.target_endpoint, state: admissionOf(failure), code: failure.code,
      });
      const stored = await this.#ctx.storage.transaction(tx => tx.get<StoredOutbound>(outboundKey(request.message.sender, request.message.id)));
      return {
        admission: stored?.state ?? admissionOf(failure),
        ...(stored?.state === 'accepted' ? {} : { error: failure }),
        ...(stored?.receipt === undefined ? (record.receipt === undefined ? {} : { receipt: record.receipt }) : { receipt: stored.receipt }),
      };
    }
  }

  /**
   * Deliver, then wait on the one thing that can answer: a reply whose author the owner
   * authenticated. A timeout ends only this wait; it never invents a state for the
   * delivery, so a still-open record stays open for recovery.
   */
  async #await(record: StoredOutbound, waiter: Waiter, signal: AbortSignal | undefined): Promise<SendOutcome> {
    const reply = await this.#awaitOutcome(record, waiter, signal);
    if (reply.reply === undefined) return reply;
    // A body already confirmed consumed in this context is rendered as arrival facts only
    // (RFC 0008 §5). The sender's own consumption identity decides; the plan's context
    // belongs to the receiver and is never this caller's. A generic caller that names none
    // is the node itself.
    const context = record.local_context ?? this.#ctx.options.node_id;
    const consumed = await this.#alreadyConsumed(reply.reply, context);
    // The body is now in the caller's hands as a tool result: hold it until the host confirms
    // consumption, so no automatic fragment presents the same body in that context again. A
    // wait that is cancelled or times out returns no reply, so it never places a hold.
    if (!consumed) {
      const identity = entryKey(reply.reply.thread_ref, reply.reply.seq);
      const held = this.#held.get(identity) ?? new Set<string>();
      this.#held.set(identity, held.add(context));
    }
    // Returning the body does not by itself write the record: RFC 0008 §5 forbids advancing
    // consumption on a submitted tool result. The host calls `confirmConsumption` once it has
    // observed the model consume the result.
    return { ...reply, ...(consumed ? { reply_body: 'withheld' as const } : {}) };
  }

  /**
   * `confirmConsumption(entry, context?, via = 'fragment'): Promise<void>`
   *
   * The host's confirmation that this entry's body was consumed by the path it names
   * (RFC 0008 §5). One commit writes the shared consumption slot, marks every matching inbox
   * record delivered, and records the todo this consumption owes; the host's todo hook runs
   * outside that commit, and the todo is filed where the model saw the body. `context` names
   * that place; with none, the context the entry landed in decides, then this node. A failing
   * hook leaves the todo owed, so `maintain` replays it, and replaying is safe because the
   * host commits the todo it creates. This also releases the hold an awaited tool result
   * placed on the entry for that context.
   */
  async confirmConsumption(entry: Entry, context?: string, via: ConsumptionRecord['via'] = 'fragment'): Promise<void> {
    const at = new Date(this.#ctx.now()).toISOString();
    const consumedIn = await this.#ctx.storage.transaction(async tx => {
      // A row keeps the context the entry landed in; the confirmation context only fills in
      // for a row that names none.
      let original: string | undefined;
      for (const row of await tx.scan<LocalAttention>(key('inbox') + '/')) {
        const held = row.value.attention.entry;
        if (held === undefined) continue;
        if (held.thread_ref.owner !== entry.thread_ref.owner || held.thread_ref.id !== entry.thread_ref.id) continue;
        if (held.seq !== entry.seq) continue;
        original = original ?? row.value.context;
        if (row.value.injected) continue;
        const landed = row.value.context ?? context;
        await tx.put(row.key, { ...row.value, injected: true, ...(landed === undefined ? {} : { context: landed }) });
      }
      const where = context ?? original ?? this.#ctx.options.node_id;
      const slot = consumptionKey(where, entry.thread_ref, entry.seq);
      const stored = await tx.get<ConsumptionRecord>(slot);
      if (stored?.confirmed !== true) {
        await tx.put<ConsumptionRecord>(slot, {
          context: where, thread_ref: entry.thread_ref, seq: entry.seq, confirmed: true, via, at,
        });
      }
      await this.#oweTodo(tx, entry, where);
      return where;
    });
    const identity = entryKey(entry.thread_ref, entry.seq);
    const held = this.#held.get(identity);
    if (held !== undefined) {
      held.delete(consumedIn);
      if (held.size === 0) this.#held.delete(identity);
    }
    this.#trace({ event: 'consumption.confirmed', thread: entry.thread_ref, seq: entry.seq, context: consumedIn, via });
    await this.#deliverTodo(entry, consumedIn);
  }

  /** `confirmToolResult(entry, context?): Promise<void>`: the same settlement for an awaited tool result. */
  async confirmToolResult(entry: Entry, context?: string): Promise<void> {
    await this.confirmConsumption(entry, context, 'tool_result');
  }

  /**
   * `isWaitingReply(entry, context?): boolean`
   *
   * Whether this entry answers an `await` that owns its body here, optionally only one whose
   * sender-local context is `context`: a waiter still running, or a reply already returned as
   * a tool result and not yet confirmed consumed. A host defers the automatic fragment on
   * `true`, because the tool result either holds or already presented the same body. A
   * cancelled wait returns no reply, so it holds nothing.
   */
  isWaitingReply(entry: Entry, context?: string): boolean {
    for (const waiter of this.#waiters.values()) {
      if (waiter.expected_author !== entry.authenticated_author || !isReply(waiter.original, entry)) continue;
      if (context !== undefined && waiter.local_context !== context) continue;
      return true;
    }
    const held = this.#held.get(entryKey(entry.thread_ref, entry.seq));
    return held !== undefined && (context === undefined || held.has(context));
  }

  async #awaitOutcome(record: StoredOutbound, waiter: Waiter, signal: AbortSignal | undefined): Promise<SendOutcome> {
    // The request and the wait start together: a reply that is already durable here, or that
    // lands while the request is still in flight, answers this call without waiting for the
    // response. The request keeps running and records its admission when it lands.
    const budget = this.#attempt(record);
    const deadline = Date.now() + budget;
    const sending = this.#transmit(record, signal);
    const first = await this.#watch(record, waiter, signal, budget, sending);
    if ('failed' in first) {
      if (signal?.aborted) return { admission: record.state, wait: 'cancelled' };
      throw first.failed;
    }
    if ('replied' in first) return { admission: record.state, ...(record.receipt === undefined ? {} : { receipt: record.receipt }), reply: first.replied, wait: 'answered' };
    if ('evidence' in first) return { admission: record.state, ...(record.receipt === undefined ? {} : { receipt: record.receipt }), reply: first.evidence, wait: 'answered' };
    if ('cancelled' in first) return { admission: record.state, wait: 'cancelled' };

    // The wait bound can pass just before the request's own deadline fires. Its outcome still
    // belongs to this call, so collect it rather than leave the attempt unobserved.
    const delivered = 'delivered' in first ? first.delivered : await sending;
    if (delivered.admission !== 'accepted') return delivered;

    // Admitted but unanswered: only a reply or the remaining deadline can end this.
    const receipt = delivered.receipt !== undefined
      ? { receipt: delivered.receipt }
      : record.receipt === undefined ? {} : { receipt: record.receipt };
    const remaining = Math.max(1, deadline - Date.now());
    const second = await this.#watch(record, waiter, signal, remaining, undefined);
    if ('failed' in second) {
      if (signal?.aborted) return { admission: 'accepted', ...receipt, wait: 'cancelled' };
      throw second.failed;
    }
    if ('replied' in second) return { admission: 'accepted', ...receipt, reply: second.replied, wait: 'answered' };
    if ('evidence' in second) return { admission: 'accepted', ...receipt, reply: second.evidence, wait: 'answered' };
    if ('cancelled' in second) return { admission: 'accepted', ...receipt, wait: 'cancelled' };
    // The budget passed: one last look at durable evidence, then stop asking.
    const late = await this.#evidence(record);
    if (late !== undefined) return { admission: 'accepted', ...receipt, reply: late, wait: 'answered' };
    return { admission: 'accepted', ...receipt, wait: 'timed_out' };
  }

  /**
   * One bounded watch over every signal that can end a wait: a live reply, durable reply
   * evidence, the request settling, cancellation, or the deadline. Exactly one timer and one
   * abort listener exist, and both are released whichever signal wins.
   */
  async #watch(
    record: StoredOutbound, waiter: Waiter, signal: AbortSignal | undefined, budget: number,
    sending: Promise<SendOutcome> | undefined,
  ): Promise<{ replied: Entry } | { evidence: Entry } | { delivered: SendOutcome } | { cancelled: true } | { failed: unknown } | { elapsed: true }> {
    type Outcome =
      | { replied: Entry } | { evidence: Entry } | { delivered: SendOutcome }
      | { cancelled: true } | { failed: unknown } | { elapsed: true };
    const evidence = this.#evidence(record);
    return new Promise<Outcome>(resolve => {
      let done = false;
      let timer: ReturnType<typeof setTimeout> | undefined;
      const onAbort = () => finish({ cancelled: true });
      const finish = (outcome: Outcome) => {
        if (done) return;
        done = true;
        clearTimeout(timer);
        signal?.removeEventListener('abort', onAbort);
        resolve(outcome);
      };
      timer = setTimeout(() => finish({ elapsed: true }), budget);
      if (signal?.aborted) finish({ cancelled: true });
      else signal?.addEventListener('abort', onAbort, { once: true });
      void waiter.outcome.then(entry => finish({ replied: entry }));
      void evidence.then(entry => { if (entry !== undefined) finish({ evidence: entry }); }, error => finish({ failed: error }));
      if (sending !== undefined) {
        void sending.then(delivered => finish({ delivered }), error => finish({ failed: error }));
      }
    });
  }

  /**
   * Acceptance is final. The digest is written with every state change, so recovery and
   * replay keep recognising the message identity instead of losing it to a bare record.
   */
  async #commit(record: StoredOutbound, change: Pick<Outbound, 'state' | 'receipt' | 'error'>): Promise<void> {
    const request = record.request;
    const digest = record.digest ?? messageDigest(request);
    await this.#ctx.storage.transaction(async tx => {
      const slot = outboundKey(request.message.sender, request.message.id);
      const current = await tx.get<StoredOutbound>(slot);
      if (current?.state === 'accepted') return;
      const next: StoredOutbound = { ...record, ...change, digest };
      if (next.state === 'accepted') delete next.error;
      await tx.put(slot, next);
    });
  }

  /** Legal reply evidence already on this node for the given original (RFC 0005 §3). */
  async #evidence(record: StoredOutbound): Promise<Entry | undefined> {
    const expected = record.expected_author;
    const found = await this.#ctx.storage.transaction(async tx =>
      (await tx.scan<LocalAttention>(key('inbox') + '/'))
        .map(row => row.value.attention.entry)
        .filter((entry): entry is Entry => entry !== undefined && isReply(record.request.message, entry) && entry.authenticated_author === expected));
    return found.sort((a, b) => (decimal(a.seq) < decimal(b.seq) ? -1 : decimal(a.seq) > decimal(b.seq) ? 1 : 0))[0];
  }

  /** Registered before the network call, so a reply that lands first still finds its waiter. */
  #register(record: StoredOutbound): Waiter {
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

  /** Releases every live await whose original this entry answers, in this process. */
  #settle(entry: Entry): void {
    let settled = 0;
    for (const waiter of this.#waiters.values()) {
      if (waiter.expected_author === entry.authenticated_author && isReply(waiter.original, entry)) {
        waiter.settle(entry);
        settled++;
      }
    }
    if (settled > 0) this.#trace({ event: 'await.settled', thread: entry.thread_ref, seq: entry.seq, waiters: settled });
  }

  /**
   * Owner-authenticated evidence only: the entry's own claims grant nothing. `owner` is
   * the node the evidence came from, so a remote owner's attention list attests just as
   * strongly as this node's own threads do, and nothing else does. `local` is the copy this
   * node already holds, used when the owner's row carries no body.
   */
  #attest(attention: Attention, owner: string, local?: Entry): boolean {
    if (attention.recipient !== this.#ctx.options.node_id) return false;
    if (attention.thread_ref?.owner !== owner) return false;
    const entry = attention.entry ?? local;
    if (entry === undefined) return attention.disposition === 'expired';
    const ref = attention.thread_ref;
    return entry.thread_ref.owner === ref.owner
      && entry.thread_ref.id === ref.id
      && entry.seq === attention.seq
      && entry.message.sender === attention.message_ref.sender
      && entry.message.id === attention.message_ref.id
      && entry.authenticated_author === attention.authenticated_author
      && entry.message.sender === entry.authenticated_author
      && typeof entry.authenticated_author === 'string' && entry.authenticated_author.length > 0
      && /^(0|[1-9][0-9]*)$/.test(entry.seq);
  }

  async #ack(ref: ThreadRef, obligation_id: string): Promise<void> {
    const mutation: Mutation = { idempotency_key: this.#ctx.id(), retry_until: this.#window(undefined) };
    const path = `/threads/${encodeURIComponent(ref.owner)}/${encodeURIComponent(ref.id)}/attention/${encodeURIComponent(obligation_id)}/ack`;
    let acked: { thread: ThreadRef; seq: string } | undefined;
    try {
      if (ref.owner === this.#ctx.options.node_id) await this.#messages.ack(this.#auth(), ref, obligation_id, mutation);
      else await this.#remote(ref.owner, 'POST', path, mutation, undefined, this.#ctx.options.wait_ms ?? 60_000);
    } catch {
      // Ack is idempotent and reconciliation re-sends it; the entry stays durable either way.
      return;
    }
    await this.#ctx.storage.transaction(async tx => {
      const stored = await tx.get<LocalAttention>(inboxKey(obligation_id));
      if (stored !== undefined && !stored.attention.acked) {
        await tx.put(inboxKey(obligation_id), { ...stored, attention: { ...stored.attention, acked: true } });
        acked = { thread: stored.attention.thread_ref, seq: stored.attention.seq };
      }
    });
    if (acked !== undefined) this.#trace({ event: 'inbound.acked', obligation: obligation_id, thread: acked.thread, seq: acked.seq });
  }

  /**
   * Consumed is recorded only after the host observed the entry in the context. The
   * context is the obligation's own, then the plan's, then this node; the host may
   * answer with the context it really used, and that is the one the todo follows.
   */
  async #consume(entry: Entry, record: LocalAttention): Promise<void> {
    const runtime = this.#ctx.options.plugins.runtime;
    if (runtime === undefined) { this.#injectTrace(entry, record, 'skipped', 'no-runtime'); return; }
    if (record.injected) { this.#injectTrace(entry, record, 'skipped', 'injected'); return; }
    const related = entry.message.meta?.in_reply_to;
    const outbound = related?.sender === this.#ctx.options.node_id
      ? await this.#ctx.storage.transaction(tx => tx.get<StoredOutbound>(outboundKey(related.sender, related.id)))
      : undefined;
    const replyContext = outbound !== undefined && outbound.expected_author === entry.authenticated_author
      && isReply(outbound.request.message, entry) ? outbound.local_context ?? this.#ctx.options.node_id : undefined;
    const requested = replyContext ?? record.context ?? record.attention.plan?.context ?? this.#ctx.options.node_id;
    // An await that owns this reply's body in that context either still holds the call or
    // already returned it as a tool result. Presenting the same body as a fragment here
    // would duplicate it; the await's confirmation releases the hold.
    if (this.isWaitingReply(entry, requested)) { this.#injectTrace(entry, record, 'waiting', 'await-hold', requested); return; }
    // The entry may already be confirmed consumed in that context through an awaited tool
    // result; presenting it again as a fragment is the duplicate RFC 0008 §5 forbids. The
    // shared record and the todo it owes are then all that is left to write.
    if (await this.#alreadyConsumed(entry, requested)) {
      this.#injectTrace(entry, record, 'consumed', 'already-consumed', requested);
      await this.confirmConsumption(entry, requested, 'tool_result');
      return;
    }
    const injected = await runtime.inject(entry, requested, record.attention.plan);
    this.#injectTrace(entry, record, injected.outcome ?? (injected.consumed ? 'consumed' : 'delivered'), undefined, injected.context);
    if (!injected.consumed) return;
    await this.confirmConsumption(entry, injected.context, 'fragment');
  }

  /**
   * Records the todo this consumption owes, inside the commit that confirms the consumption.
   * A host without a todo hook owes none, so no record accrues for it.
   */
  async #oweTodo(tx: Transaction, entry: Entry, context: string): Promise<void> {
    if (this.#ctx.options.plugins.runtime?.todo === undefined) return;
    const slot = todoKey(context, entry.thread_ref, entry.seq);
    if (await tx.get<PendingTodo>(slot) === undefined) {
      await tx.put<PendingTodo>(slot, { entry, context, done: false });
    }
  }

  /**
   * Runs the host's todo hook for one owed todo, then marks it done. A failing hook leaves
   * the record pending for `maintain`; delivering twice is safe because the host commits
   * the todo it creates.
   */
  async #deliverTodo(entry: Entry, context: string): Promise<void> {
    const runtime = this.#ctx.options.plugins.runtime;
    if (runtime?.todo === undefined) return;
    const slot = todoKey(context, entry.thread_ref, entry.seq);
    const stored = await this.#ctx.storage.transaction(tx => tx.get<PendingTodo>(slot));
    if (stored === undefined || stored.done) return;
    await runtime.todo(entry, context);
    await this.#ctx.storage.transaction(async tx => {
      const current = await tx.get<PendingTodo>(slot);
      if (current !== undefined) await tx.put<PendingTodo>(slot, { ...current, done: true });
    });
  }

  /** Whether the named context already confirmed this entry through any delivery path. */
  async #alreadyConsumed(entry: Entry, context: string): Promise<boolean> {
    const stored = await this.#ctx.storage.transaction(tx => tx.get<ConsumptionRecord>(consumptionKey(context, entry.thread_ref, entry.seq)));
    return stored?.confirmed === true;
  }

  /** Every owner + thread this node knows, from accepted grant notices plus its own threads. */
  async #knownRefs(): Promise<ThreadRef[]> {
    const granted = await this.#ctx.storage.transaction(async tx =>
      (await tx.scan<{ thread_ref: ThreadRef }>(key('known') + '/')).map(row => row.value.thread_ref));
    const refs = new Map<string, ThreadRef>();
    for (const ref of granted) refs.set(key(ref.owner, ref.id), ref);
    const owned = await this.#allPages(cursor => this.#threads.list(this.#auth(), { page_size: 100, ...(cursor === undefined ? {} : { cursor }) }));
    for (const thread of owned) refs.set(key(thread.thread_ref.owner, thread.thread_ref.id), thread.thread_ref);
    return [...refs.values()];
  }

  /** Every item of a paged listing, or a reported failure when the list never terminates. */
  async #allPages<T>(load: (cursor: string | undefined) => Promise<Page<T>>): Promise<T[]> {
    const all: T[] = [];
    let cursor: string | undefined;
    for (let fetched = 0; fetched < MAX_CURSOR_PAGES; fetched++) {
      const page = await load(cursor);
      all.push(...page.items);
      cursor = page.next_cursor ?? undefined;
      if (cursor === undefined) return all;
    }
    return fail('cursor_expired', 'The list did not terminate.');
  }

  /** The owner's authenticated attention list: remote when the owner is another node. */
  async #attentionOf(ref: ThreadRef, cursor: string | undefined, signal?: AbortSignal): Promise<Page<Attention>> {
    if (ref.owner === this.#ctx.options.node_id) {
      const options: PageOptions = { page_size: 100, ...(cursor === undefined ? {} : { cursor }) };
      return this.#messages.attention(this.#auth(), ref, options);
    }
    const path = `/threads/${encodeURIComponent(ref.owner)}/${encodeURIComponent(ref.id)}/attention`
      + (cursor === undefined ? '' : `?cursor=${encodeURIComponent(cursor)}`);
    return this.#remote<Page<Attention>>(ref.owner, 'GET', path, undefined, signal, this.#ctx.options.wait_ms ?? 60_000);
  }

  async #reconcileThread(ref: ThreadRef, signal?: AbortSignal): Promise<void> {
    let cursor: string | undefined;
    for (let fetched = 0; fetched < MAX_CURSOR_PAGES; fetched++) {
      if (signal?.aborted) return;
      const page = await this.#attentionOf(ref, cursor, signal);
      for (const attention of page.items) {
        if (signal?.aborted) return;
        // The list answers for the thread it was asked about; anything else is not evidence
        // for this thread, however well-formed it looks.
        if (attention.thread_ref?.id !== ref.id) continue;
        await this.#store(ref, attention);
      }
      cursor = page.next_cursor ?? undefined;
      if (cursor === undefined) return;
    }
    fail('cursor_expired', 'The attention list did not terminate.');
  }

  /**
   * Merges an owner's attention row into the local record, then acks and consumes it.
   * An owner that strips the body never takes back a copy this node already holds
   * (RFC 0003 §2), and an `expired` disposition marks that copy without deleting it.
   */
  async #store(ref: ThreadRef, attention: Attention): Promise<void> {
    const obligation_id = typeof attention.obligation_id === 'string' && attention.obligation_id.length > 0
      ? attention.obligation_id
      : key(ref.owner, ref.id, attention.message_ref.sender, attention.message_ref.id, this.#ctx.options.node_id);
    const stored = await this.#ctx.storage.transaction(async tx => {
      const previous = await tx.get<LocalAttention>(inboxKey(obligation_id));
      // An owner may strip the body from its list; the copy this node already holds is then
      // the evidence to check the row against, and it is never discarded (RFC 0003 §2).
      const local = previous?.attention.entry;
      if (!this.#attest(attention, ref.owner, local)) return undefined;
      const merged: LocalAttention = {
        attention: { ...attention, obligation_id, entry: attention.entry ?? local },
        ...(previous?.context === undefined ? {} : { context: previous.context }),
        injected: previous?.injected === true,
      };
      if (previous !== undefined && canonical(previous.attention) === canonical(merged.attention)) {
        return previous;
      }
      await tx.put(inboxKey(obligation_id), merged);
      return merged;
    });
    if (stored === undefined) return;
    if (!stored.attention.acked) await this.#ack(ref, obligation_id);
    if (stored.attention.entry === undefined) return;
    this.#settle(stored.attention.entry);
    if (stored.attention.disposition === 'expired') return;
    await this.#consume(stored.attention.entry, stored);
  }

  /**
   * Delivers each outstanding owner notice to its recipient, authenticated as the owner
   * that issued it. The notice stays unacked here until the recipient's own confirmation
   * round trip reaches this node's `ackGrant`.
   */
  async #deliverGrants(signal?: AbortSignal): Promise<void> {
    const budget = this.#ctx.options.wait_ms ?? 60_000;
    const notices = await this.#allPages(cursor =>
      this.#threads.notices(this.#auth(), { page_size: 100, ...(cursor === undefined ? {} : { cursor }) }));
    for (const grant of notices) {
      if (signal?.aborted) return;
      try {
        if (grant.recipient === this.#ctx.options.node_id) {
          // A node already knows the threads it hosts, so its own notice needs no delivery.
          // The recipient is this node, which is the identity `ackGrant` checks.
          await this.#threads.ackGrant(this.#auth(), grant.id, {
            idempotency_key: this.#ctx.id(), retry_until: this.#window(undefined),
          });
          continue;
        }
        await this.#remote(grant.recipient, 'POST', '/grants', { grant }, signal, budget);
      } catch {
        // The next pass retries; the notice stays outstanding until the recipient confirms.
      }
    }
  }

  /**
   * Confirms each grant this node accepted back to the owner that issued it. The owner's
   * `ackGrant` is the one that closes its notice, so the round trip ends there.
   */
  async #confirmGrants(signal?: AbortSignal): Promise<void> {
    const budget = this.#ctx.options.wait_ms ?? 60_000;
    const received = await this.#allPages(cursor =>
      this.#threads.grants(this.#auth(), { page_size: 100, ...(cursor === undefined ? {} : { cursor }) }));
    for (const grant of received) {
      if (signal?.aborted) return;
      if (grant.acked) continue;
      const mutation: Mutation = { idempotency_key: this.#ctx.id(), retry_until: this.#window(undefined) };
      try {
        if (grant.thread_ref.owner === this.#ctx.options.node_id) {
          await this.#threads.ackGrant(this.#auth(), grant.id, mutation);
        } else {
          await this.#remote(grant.thread_ref.owner, 'POST', `/grants/${encodeURIComponent(grant.id)}/ack`, mutation, signal, budget);
        }
      } catch {
        // Idempotent; the next pass sends the confirmation again.
      }
    }
  }

  async #dispatch(action: Action, options: MsgOptions): Promise<unknown> {
    const auth = this.#auth();
    const mutation = (): Mutation => ({ idempotency_key: this.#ctx.id(), retry_until: this.#window(undefined) });
    if (action.kind === 'read') {
      switch (action.scope) {
        case 'directory': return this.#directory();
        case 'threads': return this.#threads.list(auth);
        case 'inbox':
          // A thread-scoped inbox is the owner's attention list for that thread, so the
          // owner that admitted the messages is asked, exactly like reconciliation does.
          if (action.thread !== undefined) return this.#attentionOf(action.thread, action.cursor, options.signal);
          return this.#messages.inbox(auth, {
            ...(action.cursor === undefined ? {} : { cursor: action.cursor }),
            ...(action.page_size === undefined ? {} : { page_size: action.page_size }),
          });
        case 'messages':
          return this.#readThread(action, options);
        case 'message':
        case 'evidence':
          return this.#readMessage(action, options);
      }
      return fail('invalid_request', 'Unknown read scope.');
    }
    if (action.kind === 'thread') {
      switch (action.op) {
        case 'create':
          return this.#threads.create(auth, { ...mutation(), title: requireValue(action.title, 'invalid_request') });
        case 'close': return this.#threads.close(auth, requireValue(action.thread, 'invalid_request'), mutation());
        case 'list': return this.#threads.list(auth);
        case 'describe': return this.#readThreadRef(requireValue(action.thread, 'invalid_request'), options);
        case 'member_add':
          return this.#threads.member(auth, requireValue(action.thread, 'invalid_request'), requireValue(action.to, 'invalid_request'), {}, mutation());
        case 'member_remove':
          return this.#threads.member(auth, requireValue(action.thread, 'invalid_request'), requireValue(action.to, 'invalid_request'), { state: 'revoked' }, mutation());
      }
      return fail('invalid_request', 'Unknown thread operation.');
    }
    if (action.kind === 'resource') return this.#operateResource(action, options);
    if (action.kind === 'domain') {
      const domainMutation = (): DomainMutation => ({
        idempotency_key: this.#ctx.id(),
        retry_until: this.#window(undefined),
        ...(action.expected_revision === undefined ? {} : { expected_revision: action.expected_revision }),
      });
      switch (action.op) {
        case 'create': return this.#domains.create(auth, mutation());
        case 'join': return this.#domains.join(auth, requireValue(action.domain_id, 'invalid_request'), domainMutation());
        case 'leave': return this.#domains.leave(auth, requireValue(action.domain_id, 'invalid_request'), domainMutation());
        case 'list': return this.#domains.list(auth);
        case 'describe': return this.#domains.describe(auth, requireValue(action.domain_id, 'invalid_request'));
        case 'discover':
          return this.#domains.discover(auth, requireValue(action.domain_id, 'invalid_request'), options.signal);
      }
      return fail('invalid_request', 'Unknown domain operation.');
    }
    return fail('invalid_request', 'Unknown action.');
  }

  /**
   * Reads a thread over the owner's authenticated endpoint; this node's own threads are
   * read locally. A remote owner that will not serve the read is a reported failure,
   * never a silent local fallback.
   */
  async #readThread(action: Extract<Action, { kind: 'read' }>, options: MsgOptions): Promise<unknown> {
    const ref = requireValue(action.thread, 'invalid_request');
    if (ref.owner === this.#ctx.options.node_id) {
      return this.#messages.list(this.#auth(), ref, {
        attention: action.attention ?? 'thread',
        ...(action.cursor === undefined ? {} : { cursor: action.cursor }),
        ...(action.page_size === undefined ? {} : { page_size: action.page_size }),
        ...(action.seq === undefined ? {} : { since_seq: action.seq }),
        ...(action.message === undefined ? {} : { related_to: action.message }),
      });
    }
    const query: MessageQuery = {
      ...(action.cursor === undefined ? {} : { cursor: action.cursor }),
      ...(action.page_size === undefined ? {} : { page_size: action.page_size }),
      ...(action.seq === undefined ? {} : { since_seq: action.seq }),
      ...(action.message === undefined ? {} : { related_to: action.message }),
      attention: action.attention ?? 'thread',
    };
    const search = new URLSearchParams();
    for (const [name, value] of Object.entries(query)) {
      if (value === undefined) continue;
      search.set(name, typeof value === 'object' ? JSON.stringify(value) : String(value));
    }
    const path = `/threads/${encodeURIComponent(ref.owner)}/${encodeURIComponent(ref.id)}/messages`
      + (search.size === 0 ? '' : `?${search}`);
    return this.#remote(ref.owner, 'GET', path, undefined, options.signal, this.#ctx.options.wait_ms ?? 60_000);
  }

  async #readThreadRef(ref: ThreadRef, options: MsgOptions): Promise<unknown> {
    if (ref.owner === this.#ctx.options.node_id) return this.#threads.describe(this.#auth(), ref);
    return this.#remote(
      ref.owner, 'GET', `/threads/${encodeURIComponent(ref.owner)}/${encodeURIComponent(ref.id)}`,
      undefined, options.signal, this.#ctx.options.wait_ms ?? 60_000);
  }

  /**
   * One message by reference. `evidence` is this node's own delivery record; a message this
   * node appended to a thread it hosts never creates one, so its committed entry is the
   * evidence. Otherwise the owner that admitted the message is the only source, and its
   * message list is where a referenced entry is found (RFC 0004 §2).
   */
  async #readMessage(action: Extract<Action, { kind: 'read' }>, options: MsgOptions): Promise<unknown> {
    const ref = requireValue(action.message, 'invalid_request');
    if (action.scope === 'evidence') {
      const record = await this.#ctx.storage.transaction(tx => tx.get<StoredOutbound>(outboundKey(ref.sender, ref.id)));
      if (record !== undefined) return record;
      // A reply appended to a hosted thread is committed locally and delivered by that
      // commit; reporting "no evidence" for it reads as a failed send and invites a
      // duplicate. A message this node received has another sender, so it stays not_found.
      const hosted = ref.sender === this.#ctx.options.node_id
        ? await this.#ctx.storage.transaction(tx => tx.get<Entry>(key('entry-ref', ref.sender, ref.id)))
        : undefined;
      if (hosted !== undefined) {
        return { admission: 'accepted', local: true, entry: hosted, thread_ref: hosted.thread_ref, seq: hosted.seq };
      }
      fail('not_found', 'This node holds no outbound evidence for that message.');
    }
    const hosted = await this.#ctx.storage.transaction(tx => tx.get<Entry>(key('entry-ref', ref.sender, ref.id)));
    const thread = action.thread ?? hosted?.thread_ref;
    if (thread === undefined) fail('not_found', 'This node holds no evidence for that message.');
    if (thread.owner === this.#ctx.options.node_id) return this.#messages.getByRef(this.#auth(), ref);
    let cursor: string | undefined;
    for (let fetched = 0; fetched < MAX_CURSOR_PAGES; fetched++) {
      if (options.signal?.aborted) break;
      const search = new URLSearchParams({ attention: 'thread' });
      if (cursor !== undefined) search.set('cursor', cursor);
      const path = `/threads/${encodeURIComponent(thread.owner)}/${encodeURIComponent(thread.id)}/messages?${search}`;
      const page = await this.#remote<Page<Entry>>(thread.owner, 'GET', path, undefined, options.signal, this.#ctx.options.wait_ms ?? 60_000);
      const found = page.items.find(entry => entry.message.sender === ref.sender && entry.message.id === ref.id);
      if (found !== undefined) return found;
      cursor = page.next_cursor ?? undefined;
      if (cursor === undefined) break;
    }
    return fail('not_found', 'The owning node did not return that message.');
  }

  /** Resource operations route to the resource owner; this node's own resources answer locally. */
  async #operateResource(action: Extract<Action, { kind: 'resource' }>, options: MsgOptions): Promise<unknown> {
    const request = {
      idempotency_key: this.#ctx.id(),
      retry_until: this.#window(undefined),
      operation_id: this.#ctx.id(),
      operation: action.operation,
      ...(action.expected_revision === undefined ? {} : { expected_revision: action.expected_revision }),
      ...(action.data === undefined ? {} : { data: action.data }),
    };
    if (action.resource.owner === this.#ctx.options.node_id) {
      return this.#resources.operate(this.#auth(), action.resource, request, options.signal);
    }
    const path = `/resources/${encodeURIComponent(action.resource.owner)}/${encodeURIComponent(action.resource.id)}/operations`;
    return this.#remote(action.resource.owner, 'POST', path, request, options.signal, this.#ctx.options.wait_ms ?? 60_000);
  }

  /** Local facts read without a peer: this node's identity, description, threads and inbox. */
  async #directory(): Promise<{
    node: string; description: Json; threads: Page<Thread>; inbox: Page<LocalAttention>;
    domains: Page<DomainMembership>;
  }> {
    const plugins = this.#ctx.options.plugins;
    return {
      node: this.#ctx.options.node_id,
      description: plugins.directory === undefined ? null : await plugins.directory(),
      threads: await this.#threads.list(this.#auth(), { page_size: 100 }),
      inbox: await this.#messages.inbox(this.#auth(), { page_size: 100 }),
      domains: await this.#domains.list(this.#auth(), { page_size: 100 }),
    };
  }
}

