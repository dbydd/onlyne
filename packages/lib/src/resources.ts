import type {
  Auth, Failure, Handover, Json, Mutation, OperationRecord, OperationRequest, OperationResult,
  Page, PageOptions, Resource, ResourceEvent, ResourceRef, Transaction,
} from './types.js';
import type { Context } from './core.js';
import { OnlyneError, decimal, fail, key, page, requireValue, same, utc } from './core.js';

/** An accepted handover invitation, keyed by its operation ID. */
export interface HandoverInvitation extends Handover {
  record: OperationRecord;
  accepted_frozen: Resource;
}

export interface ResourceService {
  register(auth: Auth, resource: Resource): Promise<Resource>;
  list(auth: Auth, options?: PageOptions): Promise<Page<Resource>>;
  describe(auth: Auth, ref: ResourceRef): Promise<Resource>;
  operate(auth: Auth, ref: ResourceRef, request: OperationRequest, signal?: AbortSignal): Promise<OperationResult>;
  operation(auth: Auth, ref: ResourceRef, id: string): Promise<OperationRecord>;
  operations(auth: Auth, ref: ResourceRef, options?: PageOptions): Promise<Page<OperationRecord>>;
  receive(auth: Auth, ref: ResourceRef, input: { source_id: string; data: Json; expires_at?: string }): Promise<ResourceEvent>;
  events(auth: Auth, ref: ResourceRef, options?: PageOptions): Promise<Page<ResourceEvent>>;
  event(auth: Auth, ref: ResourceRef, id: string): Promise<ResourceEvent>;
  ack(auth: Auth, ref: ResourceRef, id: string, mutation: Mutation): Promise<ResourceEvent>;
  decideHandover(auth: Auth, ref: ResourceRef, id: string, accepted: boolean, mutation: Mutation): Promise<Handover>;
  invitations(auth: Auth, options?: PageOptions): Promise<Page<HandoverInvitation>>;
  recover(): Promise<void>;
}

const SCOPES: Record<'events' | 'results' | 'control', true> = { events: true, results: true, control: true };
type Scope = keyof typeof SCOPES;
const ALL_SCOPES: Scope[] = ['events', 'results', 'control'];
/** `handover` is the protocol's own operation: it runs through `decideHandover`, never through the driver. */
const RESERVED: Record<string, true> = { handover: true };
/** How long one caller waits on the driver before answering `unknown`; the external call keeps running. */
const DRIVER_WAIT_MS = 30_000;

type Settlement = Pick<OperationResult, 'state' | 'data' | 'error'>;
interface Intent { record: OperationRecord; settle: boolean }
interface PendingEvent { event: ResourceEvent }
type RowKind = 'resource' | 'operation' | 'event' | 'source' | 'pending' | 'handover';
interface Row { ref: ResourceRef; kind: RowKind }

const resourceKey = (ref: ResourceRef) => key('resource', ref.owner, ref.id);
const operationKey = (ref: ResourceRef, id: string) => key('resource', ref.owner, ref.id, 'operation', id);
const eventKey = (ref: ResourceRef, id: string) => key('resource', ref.owner, ref.id, 'event', id);
const sourceKey = (ref: ResourceRef, source: string) => key('resource', ref.owner, ref.id, 'source', source);
const pendingKey = (ref: ResourceRef) => key('resource', ref.owner, ref.id, 'pending');
const handoverKey = (ref: ResourceRef, id: string) => key('resource', ref.owner, ref.id, 'handover', id);
const operationsPrefix = (ref: ResourceRef) => key('resource', ref.owner, ref.id, 'operation');
const eventsPrefix = (ref: ResourceRef) => key('resource', ref.owner, ref.id, 'event');
/** Idempotency is scoped per resource: the same platform event or operation ID in two resources must not collide. */
const resourceScope = (ref: ResourceRef) => `${ref.owner}/${ref.id}`;

const EXTENSIONS: Record<string, RowKind> = {
  operation: 'operation', event: 'event', source: 'source', pending: 'pending', handover: 'handover',
};

/** `key` encodes each part and joins with literal slashes, so rows are recognised by decoding parts, never by matching an encoded fragment. */
function parseRow(path: string): Row | undefined {
  const parts = path.split('/');
  if (parts[0] !== 'resource' || parts.length < 3 || parts.length > 5) return undefined;
  const ref = { owner: decodeURIComponent(parts[1]!), id: decodeURIComponent(parts[2]!) };
  if (parts.length === 3) return { ref, kind: 'resource' };
  const kind = EXTENSIONS[decodeURIComponent(parts[3]!)];
  if (kind === undefined) return undefined;
  if (kind === 'pending') return parts.length === 4 ? { ref, kind } : undefined;
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

/** Read access: a reader or the current assignee. Anyone else gets `not_found`, never existence. */
function mayRead(resource: Resource, actor: string): boolean {
  return resource.readers.includes(actor) || resource.assignee === actor;
}

function mayOperate(resource: Resource, actor: string, operation: string): boolean {
  if (operation === 'describe' || operation === 'read' || operation === 'list_events') return mayRead(resource, actor);
  if (operation === 'send') return resource.assignee === actor;
  // RFC 0006 §5: the initiator is the current assignee or a representative the resource explicitly authorizes.
  if (operation === 'handover') return resource.assignee === actor || resource.controllers.includes(actor);
  return resource.controllers.includes(actor);
}

/** Absent scope moves every right; a list names exactly the rights that move, and everything else keeps its members. */
function handoverScopes(raw: unknown): Scope[] {
  if (raw === undefined) return [];
  if (!Array.isArray(raw)) fail('invalid_request', 'The handover scope is a list of permission names.');
  const scopes: Scope[] = [];
  for (const value of raw) {
    if (typeof value !== 'string' || !SCOPES[value as Scope]) fail('invalid_request', 'Unknown handover scope.');
    scopes.push(value as Scope);
  }
  return scopes;
}

function visible(resource: Resource, actor: string): boolean {
  return mayRead(resource, actor) || resource.controllers.includes(actor)
    || resource.event_recipients.includes(actor) || resource.result_recipients.includes(actor);
}

function union(target: string[], add: string[]): string[] {
  return Array.from(new Set([...target, ...add]));
}

/** Event, result, control, and read rights travel together with the assignee (RFC 0012 authorize table). */
function normalize(resource: Resource): Resource {
  const assignee = resource.assignee === undefined ? [] : [resource.assignee];
  return {
    ...resource,
    readers: union(resource.readers, assignee),
    controllers: union(resource.controllers, assignee),
    event_recipients: union(resource.event_recipients, assignee),
    result_recipients: union(resource.result_recipients, assignee),
  };
}

/**
 * The scopes a handover names move their list to the target and drop the previous assignee, so the old handler keeps
 * no new control or delivery rights; lists it did not name keep every member. The previous handler stays a reader:
 * it keeps the material it already worked on and still learns the binding it handed over (0006 §7).
 */
function transfer(resource: Resource, target: string, scopes: Scope[]): Resource {
  const previous = resource.assignee;
  // The handler role moves with control; a partial handover that leaves control alone keeps the current handler.
  const next: Resource = { ...resource, ...(scopes.includes('control') ? { assignee: target } : {}) };
  const withTarget = (list: string[]): string[] => (list.includes(target) ? list : [...list, target]);
  const withoutPrevious = (list: string[]): string[] => list.filter(member => member !== previous);
  if (scopes.includes('control')) {
    next.controllers = withTarget(withoutPrevious(next.controllers));
    next.readers = withTarget(next.readers);
  }
  if (scopes.includes('events')) next.event_recipients = withTarget(withoutPrevious(next.event_recipients));
  if (scopes.includes('results')) next.result_recipients = withTarget(withoutPrevious(next.result_recipients));
  return next;
}

export function createResourceService(ctx: Context): ResourceService {
  const drivers = ctx.options.plugins.resources;
  const mayHandover = ctx.options.plugins.mayHandover;

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

  /** A named target reads that one operation back; the invitation itself grants no resource access (0006 §5). */
  const invitedTarget = async (ref: ResourceRef, id: string, actor: string): Promise<boolean> => {
    const invitation = await ctx.storage.transaction(async tx => tx.get<HandoverInvitation>(handoverKey(ref, id)));
    return invitation !== undefined && invitation.target === actor;
  };

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
   * Every permission list the handover does not move must still equal the snapshot taken at acceptance. The lists it
   * does move are rebuilt from the fresh resource at commit, so only the untouched ones need this comparison.
   */
  function frozenStillHolds(fresh: Resource, frozen: Resource, scopes: Scope[]): boolean {
    const lists: Array<[keyof Resource & ('controllers' | 'readers' | 'event_recipients' | 'result_recipients'), Scope]> = [
      ['controllers', 'control'], ['readers', 'control'], ['event_recipients', 'events'], ['result_recipients', 'results'],
    ];
    for (const [field, scope] of lists) {
      if (scopes.includes(scope)) continue;
      if (!same(fresh[field].slice().sort(), frozen[field].slice().sort())) return false;
    }
    return scopes.includes('control') || fresh.assignee === frozen.assignee;
  }

  async function recheck(resource: Resource, invitation: HandoverInvitation, record: OperationRecord): Promise<void> {
    if (!resource.operations.includes('handover')) fail('unsupported_delivery', 'This resource no longer hands over.');
    if (!mayHandover) return;
    if (!(await mayHandover(resource, record.actor, invitation.target))) fail('forbidden', 'The resource driver refused this handover.');
  }

  function strip(item: HandoverInvitation): Handover {
    return {
      operation_id: item.operation_id, resource_ref: item.resource_ref, initiator: item.initiator, target: item.target,
      expected_revision: item.expected_revision, decision_deadline: item.decision_deadline, accepted: item.accepted,
      state: item.state, cutoff_event_id: item.cutoff_event_id, ...(item.scope === undefined ? {} : { scope: item.scope }),
    };
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

  /** Aborted means no external effect: the revision never moved past the expected one. */
  async function abortWith(tx: Transaction, ref: ResourceRef, id: string): Promise<HandoverInvitation> {
    const current = requireValue(await tx.get<HandoverInvitation>(handoverKey(ref, id)), 'not_found');
    if (current.state !== 'pending') return current;
    const stopped: HandoverInvitation = { ...current, state: 'aborted' };
    await tx.put(handoverKey(ref, id), stopped);
    await settleWith(tx, ref, id, { state: 'aborted' });
    return stopped;
  }

  const abort = (ref: ResourceRef, id: string) => ctx.storage.transaction(async tx => abortWith(tx, ref, id));

  /** A refused commit stays pending for `recover`; the caller reads back what actually holds instead of a guessed state. */
  async function currentOr(ref: ResourceRef, id: string): Promise<HandoverInvitation> {
    return requireValue(await ctx.storage.transaction(async tx => tx.get<HandoverInvitation>(handoverKey(ref, id))));
  }

  /**
   * The single commit point: deadline, revision, initiator, backlog, frozen lists and scope are all rechecked here,
   * inside one transaction, so an acceptance cannot commit against a resource that moved since.
   */
  async function commitWith(tx: Transaction, ref: ResourceRef, id: string): Promise<HandoverInvitation> {
    const current = requireValue(await tx.get<HandoverInvitation>(handoverKey(ref, id)), 'not_found');
    if (current.state !== 'pending' || !current.accepted) return current;
    if (utc(current.decision_deadline) <= ctx.now()) fail('conflict', 'The handover decision deadline passed before it could commit.');
    const resource = requireValue(await tx.get<Resource>(resourceKey(ref)));
    if (resource.revision !== current.expected_revision) {
      throw new OnlyneError('revision_conflict', 'The resource revision changed before the handover could commit.', {
        current_revision: resource.revision,
      });
    }
    if (current.initiator !== resource.assignee && !resource.controllers.includes(current.initiator)) {
      fail('forbidden', 'The initiator no longer holds this resource.');
    }
    const backlog = await tx.get<PendingEvent[]>(pendingKey(ref));
    if (backlog && backlog.length > 0) fail('conflict', 'The resource holds undelivered events; the handover cannot commit.');
    const scopes = current.scope ?? ALL_SCOPES;
    if (!frozenStillHolds(resource, current.accepted_frozen, scopes)) {
      fail('conflict', 'The resource access lists changed after the handover was accepted.');
    }

    let cutoff: string | null = null;
    for (const { value } of rowsOf(await tx.scan<ResourceEvent>(eventsPrefix(ref)), 'event')) {
      if (cutoff === null || bySequence(value.event_id, cutoff) > 0) cutoff = value.event_id;
    }
    const next = transfer(resource, current.target, scopes);
    next.revision = nextRevision(resource);
    const done: HandoverInvitation = { ...current, state: 'committed', cutoff_event_id: cutoff };
    await tx.put(resourceKey(ref), next);
    await tx.put(handoverKey(ref, id), done);
    await settleWith(tx, ref, id, { state: 'committed', data: { revision: next.revision, cutoff_event_id: cutoff } });
    return done;
  }

  const commit = (ref: ResourceRef, id: string) => ctx.storage.transaction(async tx => commitWith(tx, ref, id));

  function nextRevision(resource: Resource): string {
    if (!resource.revision) fail('internal', 'The resource revision is missing.');
    const revision = ctx.id();
    if (revision === resource.revision) fail('internal', 'The id source reused the current resource revision.');
    return revision;
  }

  /** Retention gives every delivery obligation a terminal state; only then does the backlog that blocks handover clear. */
  async function expireBacklog(): Promise<void> {
    const rows = rowsOf(await ctx.storage.transaction(async tx => tx.scan<PendingEvent[]>(key('resource'))), 'pending')
      .filter(entry => entry.value.length > 0);
    const now = ctx.now();
    for (const { row, value } of rows) {
      if (!value.some(item => utc(item.event.expires_at) <= now)) continue;
      await ctx.storage.transaction(async tx => {
        const current = await tx.get<PendingEvent[]>(pendingKey(row.ref));
        if (!current) return;
        const kept: PendingEvent[] = [];
        for (const item of current) {
          if (utc(item.event.expires_at) > now) { kept.push(item); continue; }
          const eventPath = eventKey(row.ref, item.event.event_id);
          const stored = await tx.get<ResourceEvent>(eventPath);
          if (stored) await tx.put(eventPath, { ...stored, expired: true });
        }
        await tx.put(pendingKey(row.ref), kept);
      });
    }
  }

  async function loadPendingHints(): Promise<HandoverInvitation[]> {
    const rows = rowsOf(await ctx.storage.transaction(async tx => tx.scan<HandoverInvitation>(key('resource'))), 'handover');
    return rows.map(entry => entry.value)
      .filter(invitation => invitation.resource_ref.owner === ctx.node && invitation.state === 'pending');
  }

  return {
    /**
     * The endpoint declares the resources it serves, so registration is its own action: the caller is this node and the
     * resource is one this node owns. Repeating a registration returns the stored resource, never a second definition.
     */
    async register(auth, resource) {
      requireEndpoint(auth);
      if (resource.resource_ref.owner !== ctx.node) fail('forbidden', 'This node does not serve the requested owner.');
      if (!Array.isArray(resource.readers) || !Array.isArray(resource.event_recipients) || !Array.isArray(resource.result_recipients) || !Array.isArray(resource.controllers)) {
        fail('invalid_request', 'A resource registration carries its access lists.');
      }
      decimal(resource.event_seq);
      const defined = normalize(resource);
      return ctx.storage.transaction(async tx => {
        const stored = await tx.get<Resource>(resourceKey(defined.resource_ref));
        if (stored) {
          if (!same(stored, defined)) fail('id_conflict', 'The resource is already registered with another definition.');
          return stored;
        }
        await tx.put(resourceKey(defined.resource_ref), defined);
        return defined;
      });
    },

    async list(auth, options) {
      const items = rowsOf(await ctx.storage.transaction(async tx => tx.scan<Resource>(key('resource'))), 'resource')
        .map(entry => entry.value)
        .filter(resource => resource.resource_ref.owner === ctx.node && visible(resource, auth.actor));
      items.sort((a, b) => a.resource_ref.id < b.resource_ref.id ? -1 : a.resource_ref.id > b.resource_ref.id ? 1 : 0);
      return page(items, options ?? {}, { kind: 'resources', owner: ctx.node, actor: auth.actor }, item => item.resource_ref.id);
    },

    async describe(auth, ref) {
      ctx.owner(ref.owner);
      const resource = await live(ref);
      if (!visible(resource, auth.actor)) fail('not_found', 'The requested resource is unavailable.');
      return resource;
    },

    async operate(auth, ref, request, signal) {
      ctx.owner(ref.owner);
      const prior = await intentOf(ref, request.operation_id);
      if (prior !== undefined) return resume(ref, auth, request, prior.record);

      const resource = await live(ref);
      if (!visible(resource, auth.actor)) fail('not_found', 'The requested resource is unavailable.');
      if (!resource.operations.includes(request.operation)) fail('unsupported_delivery', 'The resource does not declare this operation.');
      if (!mayOperate(resource, auth.actor, request.operation)) fail('forbidden', 'The authenticated caller may not perform this operation.');
      if (request.expected_revision !== undefined && request.expected_revision !== resource.revision) {
        throw new OnlyneError('revision_conflict', 'The resource binding changed.', { current_revision: resource.revision });
      }
      if (RESERVED[request.operation]) return initiate(auth, ref, resource, request);
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
      if (intent.record.actor !== auth.actor && !visible(resource, auth.actor) && !(await invitedTarget(ref, id, auth.actor))) {
        fail('not_found', 'The requested operation is unavailable.');
      }
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
        if (resource.event_recipients.length === 0) fail('conflict', 'This resource has no event recipients.');
        // `event_seq` is the last assigned ID, so the next event is one past it and an ID is never reused (0006 §6).
        const eventId = (decimal(resource.event_seq) + 1n).toString();
        const event: ResourceEvent = {
          resource_ref: ref, event_id: eventId, source_id: input.source_id,
          recipients: resource.event_recipients.slice(), revision: resource.revision,
          data: input.data, at: new Date(ctx.now()).toISOString(),
          expires_at: input.expires_at ?? new Date(ctx.now() + resource.retention_ms).toISOString(),
          acknowledged: [], expired: false,
        };
        utc(event.expires_at);
        resource.event_seq = eventId;
        await tx.put(resourceKey(ref), resource);
        await tx.put(eventKey(ref, eventId), event);
        await tx.put(sourceKey(ref, input.source_id), eventId);
        const pending = (await tx.get<PendingEvent[]>(pendingKey(ref))) ?? [];
        await tx.put(pendingKey(ref), [...pending, { event }]);
        return event;
      });
    },

    async events(auth, ref, options) {
      ctx.owner(ref.owner);
      const resource = await live(ref);
      const events = (await ctx.storage.transaction(async tx => tx.scan<ResourceEvent>(eventsPrefix(ref))))
        .map(entry => entry.value)
        .filter(event => event.recipients.includes(auth.actor) || mayRead(resource, auth.actor));
      // A previous handler keeps exactly the events it was first assigned, even after the binding moved (0006 §6).
      if (!mayRead(resource, auth.actor) && events.length === 0) fail('not_found', 'The requested resource is unavailable.');
      events.sort((a, b) => bySequence(a.event_id, b.event_id));
      return page(events, options ?? {}, { kind: 'events', owner: ref.owner, id: ref.id, actor: auth.actor }, event => event.event_id, bySequence);
    },

    async event(auth, ref, id) {
      ctx.owner(ref.owner);
      const resource = await live(ref);
      const event = requireValue(await ctx.storage.transaction(async tx => tx.get<ResourceEvent>(eventKey(ref, id))));
      if (!event.recipients.includes(auth.actor) && !mayRead(resource, auth.actor)) fail('not_found', 'The requested event is unavailable.');
      return event;
    },

    async ack(auth, ref, id, mutation) {
      ctx.owner(ref.owner);
      return ctx.mutate(auth, `resource_ack:${resourceScope(ref)}`, mutation, { id }, async tx => {
        const resource = requireValue(await tx.get<Resource>(resourceKey(ref)));
        const event = requireValue(await tx.get<ResourceEvent>(eventKey(ref, id)));
        if (!event.recipients.includes(auth.actor) && !mayRead(resource, auth.actor)) fail('not_found', 'The requested event is unavailable.');
        if (event.recipients[0] !== auth.actor) fail('forbidden', 'Only the first recipient acknowledges this event.');
        const pending = await tx.get<PendingEvent[]>(pendingKey(ref));
        // Expired is a terminal state written by the retention sweep. A first-recipient ack
        // arriving afterwards keeps its receipt evidence and clears any leftover, but it
        // never revives the obligation or writes a second removal (0006 §6, 0012 §5).
        if (event.expired) {
          if (pending) await tx.put(pendingKey(ref), pending.filter(item => item.event.event_id !== id));
          if (event.acknowledged.includes(auth.actor)) return event;
          const evidenced: ResourceEvent = { ...event, acknowledged: [...event.acknowledged, auth.actor] };
          await tx.put(eventKey(ref, id), evidenced);
          return evidenced;
        }
        if (event.acknowledged.includes(auth.actor)) return event;
        const updated: ResourceEvent = { ...event, acknowledged: [...event.acknowledged, auth.actor] };
        await tx.put(eventKey(ref, id), updated);
        if (pending) await tx.put(pendingKey(ref), pending.filter(item => item.event.event_id !== id));
        return updated;
      });
    },

    async decideHandover(auth, ref, id, accepted, mutation) {
      ctx.owner(ref.owner);
      const invitation = requireValue(await ctx.storage.transaction(async tx => tx.get<HandoverInvitation>(handoverKey(ref, id))), 'not_found');
      // Only the named target decides, and only on that invitation: being invited is not resource access (0006 §5).
      if (auth.actor !== invitation.target) fail('forbidden', 'Only the named target decides the handover.');
      if (invitation.state !== 'pending') return strip(invitation);
      if (accepted && !invitation.accepted) await recheck(await live(ref), invitation, invitation.record);

      // Rejection is terminal inside the transaction; acceptance is only a recorded fact and still needs the commit.
      const decided = await ctx.mutate(auth, `handover_decide:${resourceScope(ref)}`, mutation, { id, accepted }, async tx => {
        const current = requireValue(await tx.get<HandoverInvitation>(handoverKey(ref, id)), 'not_found');
        if (current.state !== 'pending') return current;
        if (!accepted) return abortWith(tx, ref, id);
        const frozen = requireValue(await tx.get<Resource>(resourceKey(ref)));
        const held: HandoverInvitation = { ...current, accepted: true, accepted_frozen: frozen };
        await tx.put(handoverKey(ref, id), held);
        return held;
      });
      if (decided.state !== 'pending' || !decided.accepted) return strip(decided);
      if (utc(decided.decision_deadline) <= ctx.now()) return strip(await abort(ref, id));
      if (mayHandover && !(await mayHandover(decided.accepted_frozen, decided.initiator, decided.target))) return strip(await abort(ref, id));
      // The commit point runs while the caller waits. An expired prepare is settled; a live fence is reported so the
      // caller can clear the backlog, and a revision that moved under the acceptance aborts the prepare.
      try { return strip(await commit(ref, id)); } catch (error) {
        if (utc(decided.decision_deadline) <= ctx.now()) return strip(await abort(ref, id));
        if (error instanceof OnlyneError && (error.error.code === 'revision_conflict' || error.error.code === 'forbidden')) {
          await abort(ref, id);
        }
        throw error;
      }
    },

    async invitations(auth, options) {
      const rows = (await loadPendingHints()).filter(invitation => invitation.target === auth.actor || invitation.initiator === auth.actor);
      rows.sort((a, b) => byAnchor(a.operation_id, b.operation_id));
      return page(rows, options ?? {}, { kind: 'invitations', owner: ctx.node, actor: auth.actor }, invitation => invitation.operation_id);
    },

    async recover() {
      // Every record settles only while unresolved, so a repeated pass is harmless; a durable marker would leave a
      // crash between two settlements permanently unrecovered.
      await expireBacklog();
      for (const hint of await loadPendingHints()) {
        const ref = hint.resource_ref;
        const current = await currentOr(ref, hint.operation_id);
        if (current.state !== 'pending') continue;
        // The deadline limits the commit, not the decision: an expired prepare is settled, never revived (0006 §5).
        if (utc(current.decision_deadline) <= ctx.now()) { await abort(ref, hint.operation_id); continue; }
        if (!current.accepted) continue;
        const fresh = await fetchResource(ref);
        if (!fresh || (mayHandover && !(await mayHandover(current.accepted_frozen, current.initiator, current.target)))) {
          await abort(ref, hint.operation_id);
          continue;
        }
        try { await commit(ref, hint.operation_id); } catch (error) {
          // A revision that moved under the acceptance, or an initiator who lost the role, is terminal for the prepare.
          if (error instanceof OnlyneError && (error.error.code === 'revision_conflict' || error.error.code === 'forbidden')) {
            await abort(ref, hint.operation_id);
          }
          // A backlog fence stays pending until the backlog clears or the deadline passes.
        }
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
   * A retried operation never executes twice: the driver is asked for the admitted one's outcome, and silence keeps the
   * recorded state instead of manufacturing a second effect or overwriting a live claim.
   */
  async function resume(ref: ResourceRef, auth: Auth, request: OperationRequest, record: OperationRecord): Promise<OperationResult> {
    if (record.digest !== digestOf(auth.actor, ref, request)) fail('id_conflict', 'The operation ID is bound to another request.');
    if (record.result.state !== 'pending') return record.result;
    if (RESERVED[request.operation]) {
      const invitation = await currentOr(ref, request.operation_id);
      return { operation_id: request.operation_id, state: invitation.state, data: handoverJson(invitation) };
    }
    const resource = await live(ref);
    const queried = await queryOutside(resource, request.operation_id);
    if (queried !== undefined && queried.state !== 'pending') return settle(ref, request.operation_id, queried);
    return (await intentOf(ref, request.operation_id))?.record.result ?? record.result;
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
   * `handover` is the one declared operation the library runs itself (RFC 0006 §5): the target, scope, and deadline are
   * validated here, the invitation is durable before the driver hears of it, and only the target's decision commits.
   * The operation stays `pending`: the target's reply is a separate request, not part of this admission.
   */
  async function initiate(auth: Auth, ref: ResourceRef, resource: Resource, request: OperationRequest): Promise<OperationResult> {
    const data = (request.data ?? {}) as Record<string, unknown>;
    const target = data.target;
    const deadline = data.decision_deadline;
    if (typeof target !== 'string' || !target) fail('invalid_request', 'A handover names its target.');
    if (typeof deadline !== 'string') fail('invalid_request', 'A handover carries a decision_deadline.');
    utc(deadline);
    const scopes = handoverScopes(data.scope);
    const resend = data.resend === true;
    if (drivers === undefined) fail('unsupported_delivery', 'This node declares no resource driver, so it cannot deliver the invitation.');
    if (resend) await drivers.validate(resource, 'handover', request.data);

    const created = await ctx.storage.transaction(async tx => {
      const existing = await tx.get<HandoverInvitation>(handoverKey(ref, request.operation_id));
      if (existing !== undefined) {
        if (existing.record.digest !== digestOf(auth.actor, ref, request)) fail('id_conflict', 'The operation ID is bound to another request.');
        return { invitation: existing, claimed: false };
      }
      ctx.deadline(request.retry_until);
      const fresh = requireValue(await tx.get<Resource>(resourceKey(ref)));
      if (fresh.revision !== resource.revision || (request.expected_revision !== undefined && request.expected_revision !== fresh.revision)) {
        throw new OnlyneError('revision_conflict', 'The resource binding changed.', { current_revision: fresh.revision });
      }
      if (!mayOperate(fresh, auth.actor, 'handover')) fail('forbidden', 'The authenticated caller may not perform this operation.');
      const record: OperationRecord = {
        actor: auth.actor, request, digest: digestOf(auth.actor, ref, request),
        result: { operation_id: request.operation_id, state: 'pending' },
        resource_ref: ref, accepted_at: new Date(ctx.now()).toISOString(), authorized_revision: fresh.revision,
      };
      const pending: HandoverInvitation = {
        operation_id: request.operation_id, resource_ref: ref, initiator: auth.actor, target,
        expected_revision: fresh.revision, decision_deadline: deadline, accepted: false, state: 'pending',
        cutoff_event_id: null, ...(data.scope === undefined ? {} : { scope: scopes }), record, accepted_frozen: fresh,
      };
      await tx.put(operationKey(ref, request.operation_id), { record, settle: true } satisfies Intent);
      await tx.put(handoverKey(ref, request.operation_id), pending);
      return { invitation: pending, claimed: true };
    });

    const invitation = created.invitation;
    if (!created.claimed) return { operation_id: request.operation_id, state: invitation.state, data: handoverJson(invitation) };
    if (mayHandover && !(await mayHandover(invitation.accepted_frozen, invitation.initiator, invitation.target))) {
      await abort(ref, request.operation_id);
      return { operation_id: request.operation_id, state: 'aborted', error: { code: 'forbidden', message: 'The resource driver refused this handover.', retryable: false } };
    }
    // The driver delivers the invitation to the target as a message; that send is evidence, never the invitation itself,
    // so a refused or unknown delivery leaves the handover pending rather than aborting it.
    if (resend) {
      // Delivery settles nothing: the operation result describes the handover, not the message that announced it.
      try { await waitFor(drivers.execute(resource, request), ctx.options.wait_ms ?? DRIVER_WAIT_MS); } catch { /* evidence only */ }
    }
    // Announced, not committed: the revision moves only when the named target accepts and the commit lands.
    return { operation_id: request.operation_id, state: 'pending', data: handoverJson(invitation) };
  }

  function handoverJson(invitation: HandoverInvitation): Json {
    return JSON.parse(JSON.stringify(strip(invitation))) as Json;
  }
}

const unknownOutcome: Failure = {
  code: 'unavailable', message: 'The operation outcome is unknown; the resource driver did not report one.', retryable: true,
};

const waitOutcome: Failure = {
  code: 'unavailable', message: 'The local wait for the resource driver expired; the external outcome is unknown.', retryable: true,
};

type Waiting<T> = { kind: 'value'; value: T } | { kind: 'timeout' } | { kind: 'failed' };

/** Bounded local wait: it answers, it never cancels the promise it stopped waiting for. */
function waitFor<T>(work: Promise<T>, ms: number): Promise<Waiting<T>> {
  return new Promise<Waiting<T>>(resolve => {
    const timer = setTimeout(() => resolve({ kind: 'timeout' }), ms);
    work.then(
      value => { clearTimeout(timer); resolve({ kind: 'value', value }); },
      () => { clearTimeout(timer); resolve({ kind: 'failed' }); },
    );
  });
}
