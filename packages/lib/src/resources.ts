import type {
  AckReceipt, Auth, Failure, HandoverProbe, Json, LogQuery, Mutation, OperationRecord, OperationRequest,
  OperationResult, Page, PageOptions, ProbeAnswer, Resource, ResourceEvent, ResourceRef, Transaction, Transport,
} from './types.ts';
import type { Context } from './core.ts';
import { OnlyneError, decimal, fail, key, page, requireValue, same, utc } from './core.ts';

export interface ResourceService {
  register(auth: Auth, resource: Resource): Promise<Resource>;
  list(auth: Auth, options?: PageOptions): Promise<Page<Resource>>;
  describe(auth: Auth, ref: ResourceRef): Promise<Resource>;
  operate(auth: Auth, ref: ResourceRef, request: OperationRequest, signal?: AbortSignal): Promise<OperationResult>;
  operation(auth: Auth, ref: ResourceRef, id: string): Promise<OperationRecord>;
  operations(auth: Auth, ref: ResourceRef, options?: PageOptions): Promise<Page<OperationRecord>>;
  receive(auth: Auth, ref: ResourceRef, input: { source_id: string; data: Json; expires_at?: string }): Promise<ResourceEvent>;
  log(auth: Auth, ref: ResourceRef, options?: LogQuery): Promise<Page<ResourceEvent>>;
  event(auth: Auth, ref: ResourceRef, id: string): Promise<ResourceEvent>;
  ack(auth: Auth, ref: ResourceRef, id: string, mutation: Mutation): Promise<AckReceipt>;
  /** Target side of a handover: record and answer the probe an endpoint sent (RFC 0006 §4.3). */
  probe(auth: Auth, probe: HandoverProbe): Promise<ProbeAnswer>;
  /** The answer this node already recorded for a probe, read back by the initiator after a crash. */
  probeAnswer(auth: Auth, operationId: string): Promise<ProbeAnswer>;
  recover(): Promise<void>;
}

/**
 * RFC 0006 §1: `read`, `describe` and `list_events` are read-only queries. The endpoint answers
 * them from its own state, so they never reach the driver and never claim an operation id.
 */
const QUERY: Record<string, true> = { read: true, describe: true, list_events: true };
/** How long one caller waits on the driver before answering `unknown`; the external call keeps running. */
const DRIVER_WAIT_MS = 30_000;

type Settlement = Pick<OperationResult, 'state' | 'data' | 'error'>;
interface Intent { record: OperationRecord; settle: boolean }
/**
 * One handover from acceptance to settlement. The intent is durable before the probe leaves,
 * so a crash mid-handshake is recoverable: `open` is the only unfinished state, and every
 * other value is already written together with the operation's terminal result.
 */
interface HandoverIntent {
  operation_id: string; resource_ref: ResourceRef; initiator: string; target: string;
  kind: string; operations: string[]; metadata?: Json;
  expected_revision: string; deadline: string; cursor: string; digest: string;
  state: 'open' | 'committed' | 'aborted';
}
/** The target's recorded answer, keyed by operation id so a replay returns the first answer. */
interface ProbeRecord { sender: string; digest: string; answer: ProbeAnswer }
type RowKind = 'resource' | 'operation' | 'event' | 'source' | 'handover';
interface Row { ref: ResourceRef; kind: RowKind }

const resourceKey = (ref: ResourceRef) => key('resource', ref.owner, ref.id);
const operationKey = (ref: ResourceRef, id: string) => key('resource', ref.owner, ref.id, 'operation', id);
const eventKey = (ref: ResourceRef, id: string) => key('resource', ref.owner, ref.id, 'event', id);
const sourceKey = (ref: ResourceRef, source: string) => key('resource', ref.owner, ref.id, 'source', source);
const handoverKey = (ref: ResourceRef, id: string) => key('resource', ref.owner, ref.id, 'handover', id);
const probeKey = (id: string) => key('probe', id);
const operationsPrefix = (ref: ResourceRef) => key('resource', ref.owner, ref.id, 'operation');
const eventsPrefix = (ref: ResourceRef) => key('resource', ref.owner, ref.id, 'event');
/** Idempotency is scoped per resource: the same platform event or operation ID in two resources must not collide. */
const resourceScope = (ref: ResourceRef) => `${ref.owner}/${ref.id}`;

const EXTENSIONS: Record<string, RowKind> = {
  operation: 'operation', event: 'event', source: 'source', handover: 'handover',
};

/** `key` encodes each part and joins with literal slashes, so rows are recognised by decoding parts, never by matching an encoded fragment. */
function parseRow(path: string): Row | undefined {
  const parts = path.split('/');
  if (parts[0] !== 'resource' || parts.length < 3 || parts.length > 5) return undefined;
  const ref = { owner: decodeURIComponent(parts[1]!), id: decodeURIComponent(parts[2]!) };
  if (parts.length === 3) return { ref, kind: 'resource' };
  const kind = EXTENSIONS[decodeURIComponent(parts[3]!)];
  if (kind === undefined) return undefined;
  return parts.length === 5 ? { ref, kind } : undefined;
}

/** Rows of one kind with decoded keys: the resource namespace also holds operations, events and handovers. */
function rowsOf<T>(entries: Array<{ key: string; value: T }>, kind: RowKind): Array<{ row: Row; value: T }> {
  const found: Array<{ row: Row; value: T }> = [];
  for (const entry of entries) {
    const row = parseRow(entry.key);
    if (row !== undefined && row.kind === kind) found.push({ row, value: entry.value });
  }
  return found;
}

/** Event and operation cursors are decimal strings; the lexical default would order `10` before `2`. */
const bySequence = (a: string, b: string): number => {
  const left = decimal(a);
  const right = decimal(b);
  return left < right ? -1 : left > right ? 1 : 0;
};

/** `accepted_at` orders an operation list; padding keeps the millisecond string monotone under the lexical cursor. */
const anchor = (record: OperationRecord) => `${String(utc(record.accepted_at)).padStart(15, '0')}:${record.request.operation_id}`;
const byAnchor = (a: string, b: string): number => (a < b ? -1 : a > b ? 1 : 0);

/**
 * RFC 0006 §6: the only two read authorities. The binding grants the current handler its
 * working view; `log_readers` is a standing grant that a handover must not rewrite.
 */
function mayRead(resource: Resource, actor: string): boolean {
  return resource.binding.handler === actor || resource.log_readers.includes(actor);
}

/** Every operation except a query belongs to the current handler; there is no second operator (RFC 0006 §3). */
function mayOperate(resource: Resource, actor: string, operation: string): boolean {
  if (QUERY[operation]) return mayRead(resource, actor);
  return resource.binding.handler === actor;
}

/** A target's answer is only an answer if it names a position or a reason; anything else is a broken peer. */
function probeAnswerOf(value: unknown): ProbeAnswer {
  if (typeof value !== 'object' || value === null) fail('unavailable', 'The target answered a probe without a body.');
  const record = value as { answer?: unknown; cursor?: unknown; reason?: unknown };
  if (record.answer === 'accept') {
    if (typeof record.cursor !== 'string') fail('unavailable', 'The target accepted a probe without a cursor.');
    decimal(record.cursor);
    return { answer: 'accept', cursor: record.cursor };
  }
  if (record.answer === 'reject') {
    if (typeof record.reason !== 'string' || record.reason.length === 0) fail('unavailable', 'The target rejected a probe without a reason.');
    return { answer: 'reject', reason: record.reason };
  }
  fail('unavailable', 'The target answered a probe with an answer the protocol does not define.');
}

type ProbeReply = { kind: 'answer'; value: ProbeAnswer } | { kind: 'timeout' } | { kind: 'failed'; error?: unknown };
/** Only recovery can find no answer at all: the in-call probe either lands or does not. */
type ProbeOutcome = ProbeReply | { kind: 'missing' };

export function createResourceService(ctx: Context, transport: Transport): ResourceService {
  const drivers = ctx.options.plugins.resources;

  /**
   * Every request for one operation ID must be identical, actor included: the same caller retries, nobody takes over
   * the slot. The retry envelope stays out of it, so a retry may refresh its window without becoming another request.
   */
  function digestOf(actor: string, ref: ResourceRef, request: OperationRequest): string {
    return JSON.stringify({
      actor, resource_ref: ref, operation: request.operation,
      expected_revision: request.expected_revision, data: request.data,
    });
  }

  /** Registering and receiving are endpoint actions: the resource owner is the endpoint, not whichever agent asks. */
  function requireEndpoint(auth: Auth): void {
    if (auth.actor !== ctx.node) fail('forbidden', 'Only the resource endpoint registers and receives here.');
  }

  const fetchResource = (ref: ResourceRef) => ctx.storage.transaction(async tx => tx.get<Resource>(resourceKey(ref)));
  const live = async (ref: ResourceRef): Promise<Resource> => {
    const resource = requireValue(await fetchResource(ref));
    if (!same(resource.resource_ref, ref)) fail('internal', 'The stored resource does not match its key.');
    return resource;
  };
  const queryOutside = async (resource: Resource, id: string): Promise<Settlement | undefined> => {
    if (drivers === undefined) return undefined;
    try { return await drivers.query?.(resource, id); } catch { return undefined; }
  };

  const intentOf = (ref: ResourceRef, id: string) => ctx.storage.transaction(async tx => tx.get<Intent>(operationKey(ref, id)));

  function nextRevision(resource: Resource): string {
    if (!resource.revision) fail('internal', 'The resource revision is missing.');
    const revision = ctx.id();
    if (revision === resource.revision) fail('internal', 'The id source reused the current resource revision.');
    return revision;
  }

  /**
   * Admission and the claim share one transaction: a concurrent caller either finds the intent already written and only
   * reports it, or writes it itself and owns the single external effect for this operation ID.
   */
  async function claimOperation(ref: ResourceRef, auth: Auth, request: OperationRequest): Promise<{ record: OperationRecord; claimed: boolean }> {
    if (!auth.actor) fail('unauthenticated', 'Authentication is required.');
    return ctx.storage.transaction(async tx => {
      const existing = await tx.get<Intent>(operationKey(ref, request.operation_id));
      if (existing) {
        if (existing.record.digest !== digestOf(auth.actor, ref, request)) fail('id_conflict', 'The operation ID is bound to another request.');
        return { record: existing.record, claimed: false };
      }
      ctx.deadline(request.retry_until);
      const fresh = requireValue(await tx.get<Resource>(resourceKey(ref)));
      if (!mayOperate(fresh, auth.actor, request.operation)) fail('forbidden', 'The authenticated caller may not perform this operation.');
      if (request.expected_revision !== undefined && request.expected_revision !== fresh.revision) {
        throw new OnlyneError('revision_conflict', 'The resource binding changed.', { current_revision: fresh.revision });
      }
      const record: OperationRecord = {
        actor: auth.actor, request, digest: digestOf(auth.actor, ref, request),
        result: { operation_id: request.operation_id, state: 'pending' },
        resource_ref: ref, accepted_at: new Date(ctx.now()).toISOString(), authorized_revision: fresh.revision,
      };
      await tx.put(operationKey(ref, request.operation_id), { record, settle: true } satisfies Intent);
      return { record, claimed: true };
    });
  }

  /**
   * `replaceUnknown` lets a late or recovered driver outcome correct a record only the local wait gave up on; a
   * committed or aborted result is never overwritten.
   */
  async function settleWith(tx: Transaction, ref: ResourceRef, id: string, outcome: Settlement, replaceUnknown = false): Promise<OperationResult> {
    const intent = requireValue(await tx.get<Intent>(operationKey(ref, id)), 'not_found');
    const current = intent.record.result.state;
    if (current !== 'pending' && !(replaceUnknown && current === 'unknown')) return intent.record.result;
    const result: OperationResult = {
      operation_id: id, state: outcome.state,
      ...(outcome.data === undefined ? {} : { data: outcome.data }),
      ...(outcome.error === undefined ? {} : { error: outcome.error }),
    };
    // `unknown` is a state to keep asking about, not a settlement: the record stays owed to `recover`.
    await tx.put(operationKey(ref, id), { record: { ...intent.record, result }, settle: result.state === 'unknown' } satisfies Intent);
    return result;
  }

  async function settle(ref: ResourceRef, id: string, outcome: Settlement, replaceUnknown = false): Promise<OperationResult> {
    return ctx.storage.transaction(async tx => settleWith(tx, ref, id, outcome, replaceUnknown));
  }

  /** The operation's recorded result, for a path that finds the handover already settled. */
  async function resultOf(tx: Transaction, ref: ResourceRef, id: string): Promise<OperationResult> {
    return requireValue(await tx.get<Intent>(operationKey(ref, id)), 'not_found').record.result;
  }

  /**
   * Retention gives every event a terminal disposition. Expiry is written onto the event row
   * itself; the log keeps the content and the id is never reused (RFC 0006 §2).
   */
  async function expireEvents(): Promise<void> {
    const now = ctx.now();
    await ctx.storage.transaction(async tx => {
      const events = rowsOf(await tx.scan<ResourceEvent>(key('resource')), 'event');
      for (const { value } of events) {
        if (value.expired || utc(value.expires_at) > now) continue;
        await tx.put(eventKey(value.resource_ref, value.event_id), { ...value, expired: true } satisfies ResourceEvent);
      }
    });
  }

  /** One event, filtered by the caller's rights (RFC 0006 §1 query semantics). */
  async function readEvent(auth: Auth, ref: ResourceRef, id: string): Promise<ResourceEvent> {
    ctx.owner(ref.owner);
    const resource = await live(ref);
    if (!mayRead(resource, auth.actor)) fail('not_found', 'The requested resource is unavailable.');
    return requireValue(await ctx.storage.transaction(async tx => tx.get<ResourceEvent>(eventKey(ref, id))));
  }

  /** The append-only log, read by the current handler or a standing log reader (RFC 0006 §6). */
  async function readLog(auth: Auth, ref: ResourceRef, options?: LogQuery): Promise<Page<ResourceEvent>> {
    ctx.owner(ref.owner);
    const resource = await live(ref);
    if (!mayRead(resource, auth.actor)) fail('not_found', 'The requested resource is unavailable.');
    const since = options?.since;
    if (since !== undefined) decimal(since);
    const events = (await ctx.storage.transaction(async tx => tx.scan<ResourceEvent>(eventsPrefix(ref))))
      .map(entry => entry.value)
      .filter(event => since === undefined || bySequence(event.event_id, since) > 0)
      .sort((a, b) => bySequence(a.event_id, b.event_id));
    return page(
      events, options ?? {}, { kind: 'log', owner: ref.owner, id: ref.id, actor: auth.actor, since },
      event => event.event_id, bySequence,
    );
  }

  /**
   * A query operation answers from this endpoint's own state. It records no operation: a read
   * that fails must not consume an idempotency slot, and RFC 0006 §1 keeps queries out of the
   * effect path entirely.
   */
  async function query(auth: Auth, ref: ResourceRef, resource: Resource, request: OperationRequest): Promise<OperationResult> {
    const id = request.operation_id;
    // A query result travels as wire JSON: the caller reads the same bytes an HTTP response carries.
    const wire = (value: unknown): Json => JSON.parse(JSON.stringify(value)) as Json;
    if (request.operation === 'describe') return { operation_id: id, state: 'committed', data: wire(resource) };
    const data = (request.data ?? {}) as Record<string, unknown>;
    if (request.operation === 'read') {
      const eventId = data.event_id;
      if (typeof eventId !== 'string' || eventId.length === 0) fail('invalid_request', 'read names the event_id to return.');
      return { operation_id: id, state: 'committed', data: wire(await readEvent(auth, ref, eventId)) };
    }
    const options: LogQuery = {};
    if (typeof data.cursor === 'string') options.cursor = data.cursor;
    if (typeof data.page_size === 'number') options.page_size = data.page_size;
    if (typeof data.since === 'string') options.since = data.since;
    return { operation_id: id, state: 'committed', data: wire(await readLog(auth, ref, options)) };
  }

  /**
   * RFC 0006 §4.4: the single point where a handover takes effect. Deadline, revision and
   * initiator are rechecked inside this transaction, so a binding that moved under the probe
   * cannot be overwritten and only one of two racing handovers switches.
   */
  async function commitHandover(ref: ResourceRef, id: string, cursor: string): Promise<OperationResult> {
    return ctx.storage.transaction(async tx => {
      const intent = requireValue(await tx.get<HandoverIntent>(handoverKey(ref, id)), 'not_found');
      if (intent.state !== 'open') return resultOf(tx, ref, id);
      if (utc(intent.deadline) <= ctx.now()) return abortWith(tx, intent, 'deadline_passed');
      const resource = requireValue(await tx.get<Resource>(resourceKey(ref)));
      if (resource.revision !== intent.expected_revision) {
        return abortWith(tx, intent, 'revision_changed', {
          code: 'revision_conflict', message: 'The resource binding changed before the handover could commit.', retryable: false,
          details: { current_revision: resource.revision },
        });
      }
      if (resource.binding.handler !== intent.initiator) {
        return abortWith(tx, intent, 'initiator_released', {
          code: 'forbidden', message: 'The initiator no longer holds this resource.', retryable: false,
        });
      }
      // An ack during the freeze moved the cursor forward; the switch must not push it back,
      // because a cursor that moves backwards re-delivers events the handler already consumed.
      const position = bySequence(cursor, resource.binding.cursor) > 0 ? cursor : resource.binding.cursor;
      const moved: Resource = { ...resource, binding: { handler: intent.target, cursor: position }, revision: nextRevision(resource) };
      await tx.put(resourceKey(ref), moved);
      await tx.put(handoverKey(ref, id), { ...intent, state: 'committed' } satisfies HandoverIntent);
      const result: OperationResult = {
        operation_id: id, state: 'committed',
        data: { handler: moved.binding.handler, cursor: moved.binding.cursor, revision: moved.revision },
      };
      const record = requireValue(await tx.get<Intent>(operationKey(ref, id)), 'not_found');
      await tx.put(operationKey(ref, id), { record: { ...record.record, result }, settle: false } satisfies Intent);
      return result;
    });
  }

  /** Aborting leaves the binding exactly as it was: the frozen events stay owed to the original handler. */
  async function abortWith(tx: Transaction, intent: HandoverIntent, reason: string, error?: Failure): Promise<OperationResult> {
    const ref = intent.resource_ref;
    const id = intent.operation_id;
    if (intent.state !== 'open') return resultOf(tx, ref, id);
    await tx.put(handoverKey(ref, id), { ...intent, state: 'aborted' } satisfies HandoverIntent);
    const record = requireValue(await tx.get<Intent>(operationKey(ref, id)), 'not_found');
    const result: OperationResult = {
      operation_id: id, state: 'aborted', data: { reason },
      ...(error === undefined ? {} : { error }),
    };
    await tx.put(operationKey(ref, id), { record: { ...record.record, result }, settle: false } satisfies Intent);
    return result;
  }

  const abortHandover = (ref: ResourceRef, id: string, reason: string, error?: Failure) =>
    ctx.storage.transaction(async tx => abortWith(tx, requireValue(await tx.get<HandoverIntent>(handoverKey(ref, id)), 'not_found'), reason, error));

  /**
   * Why a probe never reached its target. A request the transport refuses to sign is not an
   * outage: this endpoint holds no usable pairing with that node, and only its holder can
   * create one. Reporting that as `unreachable` sends the operator looking for a network fault
   * that does not exist.
   */
  function probeFailure(error: unknown): { reason: string; error?: Failure } {
    if (!(error instanceof OnlyneError)) return { reason: 'unreachable' };
    const details = error.error.details as { required_action?: unknown } | undefined;
    return { reason: details?.required_action === undefined ? 'unreachable' : 'untrusted_target', error: error.error };
  }

  /** How long the caller waits on the probe: the local window, never past the handshake deadline. */
  function waitWindow(deadline: string): number {
    const window = ctx.options.wait_ms ?? DRIVER_WAIT_MS;
    return Math.max(0, Math.min(window, utc(deadline) - ctx.now()));
  }

  /** The probe is a signed peer request like any other: it rides the node's transport, never a bare fetch. */
  async function probeTarget(intent: HandoverIntent): Promise<ProbeReply> {
    const body: HandoverProbe = {
      operation_id: intent.operation_id, resource: intent.resource_ref, kind: intent.kind,
      operations: intent.operations, ...(intent.metadata === undefined ? {} : { metadata: intent.metadata }),
      initiator: intent.initiator, cursor: intent.cursor, deadline: intent.deadline,
    };
    const call = transport.request<unknown>(intent.target, 'POST', '/handover/probes', body);
    const waited = await waitFor(call, waitWindow(intent.deadline));
    if (waited.kind === 'timeout') return { kind: 'timeout' };
    if (waited.kind === 'failed') return { kind: 'failed', error: waited.error };
    try { return { kind: 'answer', value: probeAnswerOf(waited.value) }; } catch { return { kind: 'failed' }; }
  }

  /** The answer the target recorded, read back without creating one; `not_found` means the probe never landed. */
  async function recordedAnswer(target: string, operationId: string, deadline: string): Promise<ProbeOutcome> {
    const path = `/handover/probes/${encodeURIComponent(operationId)}`;
    const waited = await waitFor(transport.request<unknown>(target, 'GET', path), waitWindow(deadline));
    if (waited.kind === 'timeout') return { kind: 'timeout' };
    // RFC 0006 §4.5: an answer the target never recorded ends the handover, while an unreachable
    // target says nothing and leaves the intent open until its deadline.
    if (waited.kind === 'failed') {
      return waited.error instanceof OnlyneError && waited.error.error.code === 'not_found' ? { kind: 'missing' } : { kind: 'failed' };
    }
    try { return { kind: 'answer', value: probeAnswerOf(waited.value) }; } catch { return { kind: 'failed' }; }
  }

  /** A settlement that cannot be written is still answered; the record stays owed to `recover`. */
  async function report(ref: ResourceRef, id: string, outcome: Settlement, replaceUnknown = false): Promise<OperationResult> {
    try { return await settle(ref, id, outcome, replaceUnknown); } catch {
      return {
        operation_id: id,
        state: outcome.state === 'committed' ? 'committed' : 'unknown',
        ...(outcome.data === undefined ? {} : { data: outcome.data }),
      };
    }
  }

  /**
   * Local intent is durable before any external effect; the driver call is the only place the effect can happen. When
   * the caller stops waiting the answer is `unknown` and the external call keeps running: a later outcome may settle
   * the record, and nothing is ever executed a second time.
   */
  async function execute(resource: Resource, request: OperationRequest, signal?: AbortSignal): Promise<OperationResult> {
    const ref = resource.resource_ref;
    const id = request.operation_id;
    if (drivers === undefined) return report(ref, id, { state: 'unknown', error: unknownOutcome });
    let call: Promise<Omit<OperationResult, 'operation_id'>>;
    try { call = drivers.execute(resource, request, signal); } catch {
      return report(ref, id, { state: 'unknown', error: unknownOutcome });
    }
    const waited = await waitFor(call, ctx.options.wait_ms ?? DRIVER_WAIT_MS);
    if (waited.kind === 'timeout') {
      void call.then(
        value => report(ref, id, value, true),
        () => report(ref, id, { state: 'unknown', error: unknownOutcome }, true),
      ).catch(() => undefined);
      return report(ref, id, { state: 'unknown', error: waitOutcome });
    }
    if (waited.kind === 'failed') return report(ref, id, { state: 'unknown', error: unknownOutcome });
    return report(ref, id, waited.value);
  }

  /**
   * RFC 0006 §4: acceptance writes the intent and freezes the cursor before the probe leaves.
   * The probe decides, and its answer settles the operation in the same call; only a probe
   * that outlives the local wait is left to `recover`.
   */
  async function initiate(auth: Auth, ref: ResourceRef, resource: Resource, request: OperationRequest): Promise<OperationResult> {
    const data = (request.data ?? {}) as Record<string, unknown>;
    const target = data.target;
    const deadline = data.handover_deadline;
    if (typeof target !== 'string' || target.length === 0) fail('invalid_request', 'A handover names its target.');
    if (typeof deadline !== 'string') fail('invalid_request', 'A handover carries a handover_deadline.');
    utc(deadline);
    const digest = digestOf(auth.actor, ref, request);

    const created = await ctx.storage.transaction(async tx => {
      const existing = await tx.get<HandoverIntent>(handoverKey(ref, request.operation_id));
      if (existing !== undefined) {
        if (existing.digest !== digest) fail('id_conflict', 'The operation ID is bound to another request.');
        return { intent: existing, claimed: false };
      }
      ctx.deadline(request.retry_until);
      const fresh = requireValue(await tx.get<Resource>(resourceKey(ref)));
      if (fresh.revision !== resource.revision || (request.expected_revision !== undefined && request.expected_revision !== fresh.revision)) {
        throw new OnlyneError('revision_conflict', 'The resource binding changed.', { current_revision: fresh.revision });
      }
      if (fresh.binding.handler !== auth.actor) fail('forbidden', 'Only the current handler starts a handover.');
      const record: OperationRecord = {
        actor: auth.actor, request, digest,
        // There is no `pending` handover (RFC 0006 §4.5): until the probe answers, the caller
        // has no certain outcome, and that is exactly what `unknown` records.
        result: { operation_id: request.operation_id, state: 'unknown', error: probeWaitOutcome },
        resource_ref: ref, accepted_at: new Date(ctx.now()).toISOString(), authorized_revision: fresh.revision,
      };
      const intent: HandoverIntent = {
        operation_id: request.operation_id, resource_ref: ref, initiator: auth.actor, target,
        kind: fresh.kind, operations: fresh.operations.slice(),
        ...(data.metadata === undefined ? {} : { metadata: data.metadata as Json }),
        expected_revision: fresh.revision, deadline, cursor: fresh.binding.cursor, digest, state: 'open',
      };
      await tx.put(operationKey(ref, request.operation_id), { record, settle: false } satisfies Intent);
      await tx.put(handoverKey(ref, request.operation_id), intent);
      return { intent, claimed: true };
    });

    if (!created.claimed) return requireValue(await intentOf(ref, request.operation_id)).record.result;
    const intent = created.intent;
    if (utc(intent.deadline) <= ctx.now()) return abortHandover(ref, intent.operation_id, 'deadline_passed');

    const answer = await probeTarget(intent);
    if (answer.kind === 'timeout') return requireValue(await intentOf(ref, intent.operation_id)).record.result;
    if (answer.kind === 'failed') {
      const failure = probeFailure(answer.error);
      return abortHandover(ref, intent.operation_id, failure.reason, failure.error);
    }
    if (answer.value.answer === 'reject') return abortHandover(ref, intent.operation_id, answer.value.reason);
    return commitHandover(ref, intent.operation_id, answer.value.cursor);
  }

  return {
    /**
     * The endpoint declares the resources it serves, so registration is its own action: the caller is this node and the
     * resource is one this node owns. Repeating a registration returns the stored resource, never a second definition.
     */
    async register(auth, resource) {
      requireEndpoint(auth);
      if (resource.resource_ref.owner !== ctx.node) fail('forbidden', 'This node does not serve the requested owner.');
      const binding = resource.binding as unknown as { handler?: unknown; cursor?: unknown } | undefined;
      if (typeof binding?.handler !== 'string' || binding.handler.length === 0) fail('invalid_request', 'A resource registration carries its handler.');
      if (typeof binding.cursor !== 'string') fail('invalid_request', 'A resource registration carries its cursor.');
      decimal(binding.cursor);
      if (!Array.isArray(resource.log_readers)) fail('invalid_request', 'A resource registration carries its log readers.');
      decimal(resource.event_seq);
      if (!Number.isInteger(resource.retention_ms) || resource.retention_ms < 0) fail('invalid_request', 'retention_ms must be a non-negative integer.');
      return ctx.storage.transaction(async tx => {
        const stored = await tx.get<Resource>(resourceKey(resource.resource_ref));
        if (stored) {
          if (!same(stored, resource)) fail('id_conflict', 'The resource is already registered with another definition.');
          return stored;
        }
        await tx.put(resourceKey(resource.resource_ref), resource);
        return resource;
      });
    },

    async list(auth, options) {
      const items = rowsOf(await ctx.storage.transaction(async tx => tx.scan<Resource>(key('resource'))), 'resource')
        .map(entry => entry.value)
        .filter(resource => resource.resource_ref.owner === ctx.node && mayRead(resource, auth.actor));
      items.sort((a, b) => a.resource_ref.id < b.resource_ref.id ? -1 : a.resource_ref.id > b.resource_ref.id ? 1 : 0);
      return page(items, options ?? {}, { kind: 'resources', owner: ctx.node, actor: auth.actor }, item => item.resource_ref.id);
    },

    async describe(auth, ref) {
      ctx.owner(ref.owner);
      const resource = await live(ref);
      if (!mayRead(resource, auth.actor)) fail('not_found', 'The requested resource is unavailable.');
      return resource;
    },

    async operate(auth, ref, request, signal) {
      ctx.owner(ref.owner);
      const prior = await intentOf(ref, request.operation_id);
      if (prior !== undefined) return resume(ref, auth, request, prior.record);

      const resource = await live(ref);
      if (!mayRead(resource, auth.actor)) fail('not_found', 'The requested resource is unavailable.');
      if (!resource.operations.includes(request.operation)) fail('unsupported_delivery', 'The resource does not declare this operation.');
      // RFC 0006 §4.1 names the initiator first: a handover belongs to the current handler alone.
      const handover = request.operation === 'handover';
      if (handover && resource.binding.handler !== auth.actor) fail('forbidden', 'Only the current handler starts a handover.');
      // RFC 0006 §7: the revision is the send fence, so a former handler learns the binding moved
      // instead of a generic refusal. A handover has already answered its initiator above.
      if (request.expected_revision !== undefined && request.expected_revision !== resource.revision) {
        throw new OnlyneError('revision_conflict', 'The resource binding changed.', { current_revision: resource.revision });
      }
      if (!handover && !mayOperate(resource, auth.actor, request.operation)) fail('forbidden', 'The authenticated caller may not perform this operation.');
      if (handover) return initiate(auth, ref, resource, request);
      if (QUERY[request.operation]) return query(auth, ref, resource, request);
      if (drivers === undefined) fail('unsupported_delivery', 'This node declares no resource driver, so it cannot validate or execute the operation.');
      // Local validation runs before the claim: a request the driver rejects must not hold an operation ID.
      await drivers.validate(resource, request.operation, request.data);
      const claim = await claimOperation(ref, auth, request);
      if (!claim.claimed) return resume(ref, auth, request, claim.record);
      return execute(resource, request, signal);
    },

    async operation(auth, ref, id) {
      ctx.owner(ref.owner);
      const resource = await live(ref);
      const intent = requireValue(await intentOf(ref, id));
      if (intent.record.actor !== auth.actor && !mayRead(resource, auth.actor)) fail('not_found', 'The requested operation is unavailable.');
      return intent.record;
    },

    async operations(auth, ref, options) {
      ctx.owner(ref.owner);
      const resource = await live(ref);
      if (!mayRead(resource, auth.actor)) fail('not_found', 'The requested resource is unavailable.');
      // An operation record carries its requester's request body, so the list is the caller's own history.
      const records = (await ctx.storage.transaction(async tx => tx.scan<Intent>(operationsPrefix(ref))))
        .map(entry => entry.value.record)
        .filter(record => record.actor === auth.actor);
      records.sort((a, b) => byAnchor(anchor(a), anchor(b)));
      return page(records, options ?? {}, { kind: 'operations', owner: ref.owner, id: ref.id, actor: auth.actor }, anchor);
    },

    async receive(auth, ref, input) {
      ctx.owner(ref.owner);
      requireEndpoint(auth);
      if (!input.source_id) fail('invalid_request', 'An inbound event needs its platform identifier.');
      return ctx.storage.transaction(async tx => {
        const resource = requireValue(await tx.get<Resource>(resourceKey(ref)));
        // A platform event ID may repeat only within its own resource, so the source row is the deduplication point.
        const duplicated = await tx.get<string>(sourceKey(ref, input.source_id));
        if (duplicated) return requireValue(await tx.get<ResourceEvent>(eventKey(ref, duplicated)));
        // `event_seq` is the last assigned ID, so the next event is one past it and an ID is never reused (0006 §5).
        const eventId = (decimal(resource.event_seq) + 1n).toString();
        const event: ResourceEvent = {
          resource_ref: ref, event_id: eventId, source_id: input.source_id,
          data: input.data, at: new Date(ctx.now()).toISOString(),
          expires_at: input.expires_at ?? new Date(ctx.now() + resource.retention_ms).toISOString(),
          expired: false,
        };
        utc(event.expires_at);
        await tx.put(resourceKey(ref), { ...resource, event_seq: eventId } satisfies Resource);
        await tx.put(eventKey(ref, eventId), event);
        await tx.put(sourceKey(ref, input.source_id), eventId);
        return event;
      });
    },

    async log(auth, ref, options) {
      return readLog(auth, ref, options);
    },

    async event(auth, ref, id) {
      return readEvent(auth, ref, id);
    },

    /**
     * RFC 0006 §5: an ack advances the handler's cursor, nothing else. It is idempotent because
     * the cursor only moves forward, and an id that is already behind it leaves the position alone.
     */
    async ack(auth, ref, id, mutation) {
      ctx.owner(ref.owner);
      return ctx.mutate(auth, `resource_ack:${resourceScope(ref)}`, mutation, { id }, async tx => {
        const resource = requireValue(await tx.get<Resource>(resourceKey(ref)));
        if (resource.binding.handler !== auth.actor) fail('forbidden', 'Only the current handler acknowledges events.');
        requireValue(await tx.get<ResourceEvent>(eventKey(ref, id)));
        if (bySequence(id, resource.binding.cursor) <= 0) return { resource_ref: ref, cursor: resource.binding.cursor } satisfies AckReceipt;
        const moved: Resource = { ...resource, binding: { ...resource.binding, cursor: id } };
        await tx.put(resourceKey(ref), moved);
        return { resource_ref: ref, cursor: id } satisfies AckReceipt;
      });
    },

    async probe(auth, probe) {
      if (!auth.actor) fail('unauthenticated', 'Authentication is required.');
      if (!probe.operation_id) fail('invalid_request', 'A probe carries its operation id.');
      if (typeof probe.resource?.owner !== 'string' || typeof probe.resource.id !== 'string') {
        fail('invalid_request', 'A probe carries the resource reference.');
      }
      decimal(probe.cursor);
      const deadline = utc(probe.deadline);
      const digest = JSON.stringify(probe);
      const existing = await ctx.storage.transaction(async tx => tx.get<ProbeRecord>(probeKey(probe.operation_id)));
      if (existing) {
        if (existing.sender !== auth.actor || existing.digest !== digest) fail('id_conflict', 'The operation ID is bound to another probe.');
        return existing.answer;
      }
      // The policy runs outside the transaction: a host hook that reads storage would deadlock here.
      const answer = await decideProbe(probe, deadline, auth.actor);
      return ctx.storage.transaction(async tx => {
        const raced = await tx.get<ProbeRecord>(probeKey(probe.operation_id));
        if (raced) {
          if (raced.sender !== auth.actor || raced.digest !== digest) fail('id_conflict', 'The operation ID is bound to another probe.');
          return raced.answer;
        }
        await tx.put(probeKey(probe.operation_id), { sender: auth.actor, digest, answer } satisfies ProbeRecord);
        return answer;
      });
    },

    async probeAnswer(auth, operationId) {
      const record = requireValue(await ctx.storage.transaction(async tx => tx.get<ProbeRecord>(probeKey(operationId))));
      // A probe answer names a resource and its position: only the endpoint that asked may read it.
      if (record.sender !== auth.actor) fail('not_found', 'The requested probe is unavailable.');
      return record.answer;
    },

    async recover() {
      // Every record settles only while unresolved, so a repeated pass is harmless; a durable marker would leave a
      // crash between two settlements permanently unrecovered.
      await expireEvents();
      const handovers = rowsOf(await ctx.storage.transaction(async tx => tx.scan<HandoverIntent>(key('resource'))), 'handover');
      for (const { row, value } of handovers) {
        if (value.state !== 'open') continue;
        const ref = row.ref;
        // RFC 0006 §4.5: a handshake that outlived its deadline never commits.
        if (utc(value.deadline) <= ctx.now()) { await abortHandover(ref, value.operation_id, 'deadline_passed'); continue; }
        const asked = await recordedAnswer(value.target, value.operation_id, value.deadline);
        if (asked.kind === 'missing') { await abortHandover(ref, value.operation_id, 'no_answer'); continue; }
        // An unreachable target is not an answer: the intent stays open until the deadline or a later pass.
        if (asked.kind !== 'answer') continue;
        if (asked.value.answer === 'accept') await commitHandover(ref, value.operation_id, asked.value.cursor);
        else await abortHandover(ref, value.operation_id, asked.value.reason);
      }
      const intents = rowsOf(await ctx.storage.transaction(async tx => tx.scan<Intent>(key('resource'))), 'operation');
      for (const { row, value } of intents) {
        if (!value.settle) continue;
        const state = value.record.result.state;
        if (state !== 'pending' && state !== 'unknown') continue;
        const id = value.record.request.operation_id;
        const resource = await fetchResource(row.ref);
        const queried = resource === undefined ? undefined : await queryOutside(resource, id);
        const late = state === 'unknown';
        if (queried !== undefined && queried.state !== 'pending') await settle(row.ref, id, queried, late);
        else if (resource === undefined) await settle(row.ref, id, { state: 'unknown', error: unknownOutcome }, late);
      }
    },
  };

  /**
   * The target's own policy decides, and its answer is final for this operation id. A probe that
   * arrives past its deadline is refused rather than accepted late (RFC 0006 §4.3).
   */
  async function decideProbe(probe: HandoverProbe, deadline: number, sender: string): Promise<ProbeAnswer> {
    if (deadline <= ctx.now()) return { answer: 'reject', reason: 'deadline_passed' };
    const policy = ctx.options.plugins.handoverProbe;
    if (policy === undefined) return { answer: 'accept', cursor: probe.cursor };
    const answer = await policy(probe, sender);
    if (answer.answer === 'accept') {
      decimal(answer.cursor);
      return { answer: 'accept', cursor: answer.cursor };
    }
    if (answer.answer === 'reject' && typeof answer.reason === 'string' && answer.reason.length > 0) {
      return { answer: 'reject', reason: answer.reason };
    }
    fail('internal', 'The handover policy returned an answer the protocol does not define.');
  }

  /**
   * A retried operation never executes twice: the driver is asked for the admitted one's outcome, and silence keeps the
   * recorded state instead of manufacturing a second effect or overwriting a live claim.
   */
  async function resume(ref: ResourceRef, auth: Auth, request: OperationRequest, record: OperationRecord): Promise<OperationResult> {
    if (record.digest !== digestOf(auth.actor, ref, request)) fail('id_conflict', 'The operation ID is bound to another request.');
    if (record.result.state !== 'pending') return record.result;
    const resource = await live(ref);
    const queried = await queryOutside(resource, request.operation_id);
    if (queried !== undefined && queried.state !== 'pending') return settle(ref, request.operation_id, queried);
    return (await intentOf(ref, request.operation_id))?.record.result ?? record.result;
  }
}

const unknownOutcome: Failure = {
  code: 'unavailable', message: 'The operation outcome is unknown; the resource driver did not report one.', retryable: true,
};

const waitOutcome: Failure = {
  code: 'unavailable', message: 'The local wait for the resource driver expired; the external outcome is unknown.', retryable: true,
};

const probeWaitOutcome: Failure = {
  code: 'unavailable', message: 'The handover probe has not answered within the local wait; query the operation by id.', retryable: true,
};

type Waiting<T> = { kind: 'value'; value: T } | { kind: 'timeout' } | { kind: 'failed'; error: unknown };

/** Bounded local wait: it answers, it never cancels the promise it stopped waiting for. */
function waitFor<T>(work: Promise<T>, ms: number): Promise<Waiting<T>> {
  const { promise, resolve } = Promise.withResolvers<Waiting<T>>();
  const timer = setTimeout(() => resolve({ kind: 'timeout' }), ms);
  work.then(
    value => { clearTimeout(timer); resolve({ kind: 'value', value }); },
    error => { clearTimeout(timer); resolve({ kind: 'failed', error }); },
  );
  return promise;
}
