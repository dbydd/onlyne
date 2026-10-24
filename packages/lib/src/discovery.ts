import { OnlyneError, fail, key, page, type Context } from './core.js';
import { membershipRecord, validateDomainId } from './domains.js';
import type {
  DiscoveryCandidate, DiscoveryRecord, DomainId, Failure, NodeId, Page, PageOptions, ServiceDescription, Target,
} from './types.js';

/** How long candidate evidence stays current. An expired record is history, not a live address. */
const FRESH_MS = 120_000;
const PREFIX = 'discovery';

function iso(at: number): string { return new Date(at).toISOString(); }

function order(a: string, b: string): number { return a < b ? -1 : a > b ? 1 : 0; }

function position(candidate: DiscoveryCandidate): string {
  return `${candidate.node_id}\u0000${candidate.instance_id}\u0000${candidate.base_url}`;
}

function hostOf(url: URL): string { return url.hostname.replace(/^\[|\]$/g, '').toLowerCase(); }

/** Loopback may be spoken to over plain HTTP; every other peer must use HTTPS. */
function loopback(url: URL): boolean {
  const host = hostOf(url);
  return host === 'localhost' || host === '::1' || /^127\./.test(host);
}

/** True for an address another host could use. Loopback, wildcard and unscoped link-local stay home. */
function exchangeable(value: string): boolean {
  let url: URL;
  try { url = new URL(value); } catch { return false; }
  if (url.protocol !== 'http:' && url.protocol !== 'https:') return false;
  const host = hostOf(url);
  if (loopback(url) || host === '0.0.0.0' || host === '::') return false;
  if (/^169\.254\./.test(host)) return false;
  return !(/^fe80:/.test(host) && !host.includes('%'));
}

function address(value: string): URL {
  let url: URL | undefined;
  try { url = new URL(value); } catch { /* reported below */ }
  if (url === undefined) fail('invalid_request', 'A discovery address must be an absolute URL.');
  if (url.protocol !== 'http:' && url.protocol !== 'https:') fail('invalid_request', 'A discovery address must be HTTP or HTTPS.');
  if (url.search !== '' || url.hash !== '') fail('invalid_request', 'A discovery address carries no query or fragment.');
  return url;
}

function field(value: unknown, name: string): string {
  if (typeof value !== 'string' || value.length === 0) fail('unavailable', `The candidate published no ${name}.`);
  return value;
}

function isFailureRecord(value: unknown): value is Failure {
  if (value === null || typeof value !== 'object' || !('code' in value) || !('message' in value)) return false;
  return typeof value.code === 'string' && typeof value.message === 'string';
}

function names(value: unknown, what: string): string[] {
  if (!Array.isArray(value) || !value.every(entry => typeof entry === 'string')) fail('unavailable', `The candidate published unreadable ${what}.`);
  return value;
}

/**
 * A service description is the candidate's own declaration. It must speak onlyne/3, name an
 * instance, and publish a membership that agrees with the identity it claims.
 */
function published(value: unknown): ServiceDescription {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) fail('unavailable', 'The candidate returned no service description.');
  if (!('protocol' in value) || !('node_id' in value) || !('instance_id' in value) || !('base_url' in value)
    || !('features' in value) || !('authentication' in value) || !('membership' in value) || !('domain' in value)) {
    fail('unavailable', 'The candidate returned an incomplete service description.');
  }
  if (value.protocol !== 'onlyne/3') fail('unavailable', 'The candidate does not speak onlyne/3.');
  const node_id = field(value.node_id, 'node identity');
  const instance_id = field(value.instance_id, 'instance identity');
  const base_url = field(value.base_url, 'base address');
  let parsed: URL | undefined;
  try { parsed = new URL(base_url); } catch { /* reported below */ }
  if (parsed === undefined) fail('unavailable', 'The candidate published an unreadable base address.');
  if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') fail('unavailable', 'The candidate published a base address this protocol cannot use.');
  const features = names(value.features, 'features');
  const authentication = names(value.authentication, 'authentication schemes');
  const membership = membershipRecord(value.membership);
  if (membership === undefined) fail('unavailable', 'The candidate published an unreadable membership.');
  if (membership !== null && membership.node_id !== node_id) fail('unavailable', 'The candidate published a membership for another node.');
  const domain = value.domain;
  if (domain !== null && typeof domain !== 'string') fail('unavailable', 'The candidate published an unreadable region.');
  if (domain !== null && (membership === null || membership.domain_id !== domain)) fail('unavailable', 'The candidate published a region its membership does not match.');
  return {
    protocol: 'onlyne/3', node_id, instance_id, base_url,
    features, authentication, domain, membership,
  };
}

/**
 * The local discovery catalog. It holds one record per (node, address), so two addresses for the
 * same node stay two observations until a direct answer shows they are the same instance.
 *
 * Nothing here trusts a third party: a record enters the catalog as a candidate clue, and only a
 * direct `GET /description` makes it usable for routing.
 */
export class DiscoveryService {
  /** Directly verified identities in this process; lets admission policies answer without nested storage work. */
  readonly #known = new Set<NodeId>();
  constructor(private readonly ctx: Context, private readonly fetcher: (input: string | URL | Request, init?: RequestInit) => Promise<Response> = fetch) {}

  /** Whether this process has directly verified this identity at least once. */
  known(node_id: NodeId): boolean { return this.#known.has(node_id); }

  /**
   * Reads `base_url`'s service description and records what it declares. A failed observation
   * keeps the last verified identity of the address and stores the failure beside it.
   */
  async observe(base_url: string, source: string, expectedNode?: NodeId, signal?: AbortSignal): Promise<DiscoveryRecord> {
    if (source.length === 0) fail('invalid_request', 'A discovery record needs a source.');
    const url = address(base_url);
    if (url.protocol === 'http:' && !loopback(url)) fail('unavailable', 'Plain HTTP is limited to loopback; cross-host candidates must use HTTPS.');
    const at = this.ctx.now();
    let description: ServiceDescription;
    try {
      description = published(await this.#read(url, signal));
      if (expectedNode !== undefined && description.node_id !== expectedNode) {
        fail('unavailable', 'The candidate description names another node.');
      }
    } catch (error) {
      if (signal?.aborted) throw signal.reason;
      await this.#failed(base_url, source, at,
        error instanceof OnlyneError ? error.error : { code: 'internal', message: 'The candidate observation failed.', retryable: false });
      throw error;
    }
    const record: DiscoveryRecord = {
      node_id: description.node_id,
      instance_id: description.instance_id,
      base_url,
      last_seen: iso(at),
      source,
      received_at: iso(at),
      expires_at: iso(at + FRESH_MS),
      verified_at: iso(at),
      membership: description.membership,
    };
    await this.ctx.storage.transaction(async tx => { await tx.put(key(PREFIX, record.node_id, base_url), record); });
    this.#known.add(record.node_id);
    return record;
  }

  /** The public candidate exchange: verified, fresh and reachable from somewhere other than this host. */
  async list(options: PageOptions = {}): Promise<Page<DiscoveryCandidate>> {
    const at = this.ctx.now();
    const candidates = (await this.listRecords())
      .filter(record => record.verified_at !== undefined && record.instance_id !== ''
        && exchangeable(record.base_url) && Date.parse(record.expires_at) > at)
      .map(record => ({ node_id: record.node_id, instance_id: record.instance_id, base_url: record.base_url, last_seen: record.last_seen }))
      .sort((a, b) => order(position(a), position(b)));
    return page(candidates, options, { kind: 'discovery', node: this.ctx.node }, position);
  }

  /** Every observation this node holds, expired and failed ones included. */
  async listRecords(): Promise<DiscoveryRecord[]> {
    const rows = await this.ctx.storage.transaction(tx => tx.scan<DiscoveryRecord>(`${PREFIX}/`));
    return rows.map(row => row.value).sort((a, b) => order(position(a), position(b)));
  }

  /**
   * Rechecks every recorded address for one identity. Two live instances of one NodeId are a
   * conflict; the same instance at several addresses is one target. An address that no longer
   * answers is skipped, so a restarted node's new address is usable immediately.
   */
  async resolve(node_id: NodeId): Promise<Target> {
    const records = (await this.listRecords()).filter(record => record.node_id === node_id);
    if (records.length === 0) fail('not_found', 'No discovery record names this node.');
    const answers = await Promise.all(records.map(async record => {
      try { return await this.observe(record.base_url, record.source, node_id); } catch { return undefined; }
    }));
    const live = new Map<string, string[]>();
    for (const record of answers) {
      if (record === undefined) continue;
      const addresses = live.get(record.instance_id) ?? [];
      addresses.push(record.base_url);
      live.set(record.instance_id, addresses);
    }
    if (live.size > 1) fail('conflict', 'More than one live instance answers for this node.');
    const entries = [...live.entries()];
    if (entries.length === 0) fail('unavailable', 'No recorded address for this node answered.');
    const addresses = entries[0]![1].sort(order);
    return { node_id, base_url: addresses[0]! };
  }

  /** Verified, fresh candidates that declare exactly this region. */
  async candidates(domain_id: DomainId): Promise<NodeId[]> {
    validateDomainId(domain_id);
    const at = this.ctx.now();
    const nodes = new Set<NodeId>();
    for (const record of await this.listRecords()) {
      if (record.verified_at === undefined || record.instance_id === '') continue;
      if (Date.parse(record.expires_at) <= at || !exchangeable(record.base_url)) continue;
      const membership = record.membership;
      if (membership === null || membership === undefined || membership.state !== 'active' || membership.domain_id !== domain_id) continue;
      nodes.add(record.node_id);
    }
    return [...nodes].sort(order);
  }

  /** Reobserves every recorded address. One unreachable candidate commits its own failure and no other. */
  async refresh(signal?: AbortSignal): Promise<void> {
    const records = await this.listRecords();
    await Promise.all(records.map(record => this.observe(record.base_url, record.source, record.node_id, signal).then(() => undefined, () => undefined)));
    if (signal?.aborted) throw signal.reason;
  }

  async #read(url: URL, signal?: AbortSignal): Promise<unknown> {
    const target = new URL('./description', url.href.endsWith('/') ? url.href : `${url.href}/`);
    const controller = new AbortController();
    const relay = () => controller.abort(signal?.reason);
    if (signal?.aborted) controller.abort(signal.reason);
    else signal?.addEventListener('abort', relay, { once: true });
    const gate = Promise.withResolvers<never>();
    controller.signal.addEventListener('abort', () => gate.reject(controller.signal.reason), { once: true });
    const timeout = setTimeout(
      () => controller.abort(new OnlyneError('unavailable', 'The candidate description lookup timed out.')),
      this.ctx.options.wait_ms ?? 30_000,
    );
    let response: Response;
    try {
      response = await Promise.race([
        this.fetcher(target, { method: 'GET', headers: { accept: 'application/json' }, signal: controller.signal }),
        gate.promise,
      ]);
    } catch (error) {
      if (signal?.aborted) throw signal.reason;
      throw error instanceof OnlyneError ? error : new OnlyneError('unavailable', 'The candidate could not be reached.');
    } finally {
      clearTimeout(timeout);
      signal?.removeEventListener('abort', relay);
    }

    let text: string;
    try { text = await response.text(); } catch { throw new OnlyneError('unavailable', 'The candidate response could not be read.'); }
    let parsed: unknown;
    try { parsed = text === '' ? undefined : JSON.parse(text); } catch { parsed = undefined; }
    if (!response.ok) {
      const record = parsed !== null && typeof parsed === 'object' && 'error' in parsed ? parsed.error : parsed;
      if (isFailureRecord(record)) throw new OnlyneError(record.code, record.message, record.details);
      throw new OnlyneError('unavailable', `The candidate answered HTTP ${response.status} without a service description.`);
    }
    if (parsed === undefined) throw new OnlyneError('unavailable', 'The candidate returned no service description.');
    return parsed;
  }

  /** A failure is recorded beside addresses this node already observed; a clue that never verified leaves no record. */
  async #failed(base_url: string, source: string, at: number, error: Failure): Promise<void> {
    await this.ctx.storage.transaction(async tx => {
      const rows = (await tx.scan<DiscoveryRecord>(`${PREFIX}/`)).filter(row => row.value.base_url === base_url);
      for (const row of rows) await tx.put(row.key, { ...row.value, source, received_at: iso(at), error });
    });
  }
}
