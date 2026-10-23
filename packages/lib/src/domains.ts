import { fail, key, OnlyneError, page, requireValue, type Context } from './core.js';
import type { Auth, DomainDiscovery, DomainMembership, DomainMutation, Mutation, PageOptions, Transaction, Transport } from './types.js';

export function validateDomainId(value: unknown): string {
  if (typeof value !== 'string' || !/^[A-Za-z][A-Za-z0-9+.-]*:[^\s]+$/.test(value)) {
    fail('invalid_request', 'A domain_id must be an absolute URI.');
  }
  return value;
}

function membership(value: unknown, domain: string, node: string): value is DomainMembership {
  return value !== null && typeof value === 'object'
    && 'domain_id' in value && value.domain_id === domain
    && 'node_id' in value && value.node_id === node
    && 'state' in value && (value.state === 'active' || value.state === 'left')
    && 'revision' in value && typeof value.revision === 'string' && value.revision.length > 0;
}

async function activeRows(tx: Transaction, node: string): Promise<DomainMembership[]> {
  return (await tx.scan<DomainMembership>(key('domain') + '/'))
    .map(row => row.value)
    .filter(row => row.node_id === node && row.state === 'active');
}

/**
 * The one active membership a node may hold, read inside the caller's transaction.
 * Storage written by another revision can hold several; that state is refused instead of
 * silently choosing one, so the single-membership rule stays visible to the host.
 */
async function activeMembership(tx: Transaction, node: string): Promise<DomainMembership | undefined> {
  const rows = await activeRows(tx, node);
  if (rows.length > 1) fail('conflict', 'This node holds more than one active domain membership; resolve it before continuing.');
  return rows[0];
}

export class DomainService {
  constructor(private readonly ctx: Context, private readonly transport: Transport) {}

  private authenticate(auth: Auth): void {
    if (!auth.actor) fail('unauthenticated', 'Authentication is required.');
  }

  private self(auth: Auth): void {
    this.authenticate(auth);
    if (auth.actor !== this.ctx.node) fail('forbidden', 'A node may only change its own domain membership.');
  }

  async create(auth: Auth, mutation: Mutation): Promise<DomainMembership> {
    this.self(auth);
    return this.ctx.mutate(auth, 'domain.create', mutation, {}, async tx => {
      if (await activeMembership(tx, this.ctx.node)) {
        fail('conflict', 'This node already belongs to a domain; leave it before creating another.');
      }
      const domain_id = validateDomainId(`urn:onlyne:domain:${this.ctx.id()}`);
      const slot = key('domain', domain_id);
      if (await tx.get(slot)) fail('id_conflict', 'The domain identifier is already recorded here.');
      const record: DomainMembership = { domain_id, node_id: this.ctx.node, state: 'active', revision: this.ctx.id() };
      await tx.put(slot, record);
      return record;
    });
  }

  async join(auth: Auth, id: string, mutation: DomainMutation): Promise<DomainMembership> {
    return this.change(auth, id, 'active', mutation);
  }

  async leave(auth: Auth, id: string, mutation: DomainMutation): Promise<DomainMembership> {
    return this.change(auth, id, 'left', mutation);
  }

  private async change(auth: Auth, id: string, state: DomainMembership['state'], mutation: DomainMutation): Promise<DomainMembership> {
    this.self(auth);
    validateDomainId(id);
    return this.ctx.mutate(auth, `domain.${state}/${id}`, mutation, { expected_revision: mutation.expected_revision ?? null }, async tx => {
      const slot = key('domain', id);
      const current = await tx.get<DomainMembership>(slot);
      if (!current && state === 'left') fail('not_found', 'No membership is recorded for this domain.');
      // A node belongs to one domain. Cross-domain reach is a property of threads and ingress, not of membership.
      const active = await activeMembership(tx, this.ctx.node);
      if (state === 'active' && active !== undefined && active.domain_id !== id) {
        fail('conflict', 'This node already belongs to another domain; leave it first.');
      }
      if (current?.revision !== mutation.expected_revision) {
        fail('revision_conflict', 'The local domain membership changed.', current ? { current_revision: current.revision } : undefined);
      }
      if (current?.state === state) return current;
      const revision = this.ctx.id();
      if (revision === current?.revision) fail('internal', 'The revision source reused the current membership revision.');
      const record: DomainMembership = { domain_id: id, node_id: this.ctx.node, state, revision };
      await tx.put(slot, record);
      return record;
    });
  }

  async list(auth: Auth, options: PageOptions = {}) {
    this.authenticate(auth);
    return this.ctx.storage.transaction(async tx => {
      const current = await activeMembership(tx, this.ctx.node);
      const records = current === undefined ? [] : [current];
      records.sort((a, b) => a.domain_id < b.domain_id ? -1 : a.domain_id > b.domain_id ? 1 : 0);
      return page(records, options, { kind: 'domains', node: this.ctx.node, actor: auth.actor }, row => row.domain_id);
    });
  }

  async describe(auth: Auth, id: string): Promise<DomainMembership> {
    this.authenticate(auth);
    validateDomainId(id);
    return this.ctx.storage.transaction(async tx => requireValue(await tx.get<DomainMembership>(key('domain', id))));
  }

  async discover(auth: Auth, id: string, signal?: AbortSignal): Promise<DomainDiscovery> {
    this.authenticate(auth);
    validateDomainId(id);
    if ((await this.describe(auth, id)).state !== 'active') fail('not_a_member', 'This node has left the domain.');
    const candidates = this.ctx.options.plugins.domainCandidates;
    if (!candidates) fail('unsupported_delivery', 'The host provides no domain discovery source.');
    signal?.throwIfAborted();
    const nodes = [...new Set([this.ctx.node, ...await candidates(id)])].sort();
    const members: DomainMembership[] = [];
    const unavailable: string[] = [];
    for (const node of nodes) {
      signal?.throwIfAborted();
      if (typeof node !== 'string' || node.length === 0) fail('invalid_request', 'Domain candidates must be node identities.');
      if (node === this.ctx.node) {
        const current = await this.describe(auth, id);
        if (current.state === 'active') members.push(current);
        continue;
      }
      try {
        const record = await this.query(node, id, signal);
        if (!membership(record, id, node)) unavailable.push(node);
        else if (record.state === 'active') members.push(record);
      } catch (error) {
        signal?.throwIfAborted();
        if (!(error instanceof OnlyneError && error.error.code === 'not_found')) unavailable.push(node);
      }
    }
    return { domain_id: id, members, unavailable };
  }

  private async query(node: string, id: string, signal?: AbortSignal): Promise<unknown> {
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const onAbort = () => controller.abort(signal?.reason);
    signal?.addEventListener('abort', onAbort, { once: true });
    if (signal?.aborted) controller.abort(signal.reason);
    const aborted = new Promise<never>((_, reject) => {
      controller.signal.addEventListener('abort', () => reject(controller.signal.reason), { once: true });
      if (controller.signal.aborted) reject(controller.signal.reason);
    });
    timer = setTimeout(() => controller.abort(new OnlyneError('unavailable', 'Domain membership lookup timed out.')), this.ctx.options.wait_ms ?? 30_000);
    try {
      return await Promise.race([
        this.transport.request<unknown>(node, 'GET', `/domains/${encodeURIComponent(id)}`, undefined, controller.signal),
        aborted,
      ]);
    } finally {
      clearTimeout(timer);
      signal?.removeEventListener('abort', onAbort);
    }
  }
}
