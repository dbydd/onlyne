/**
 * Runtime acceptance for node groups (domains).
 *
 * A domain is an ownerless group of nodes: every node persists and serves only
 * its own DomainMembership, and a node holds at most one active membership:
 * threads and ingress are what reach across domains. Nothing here asserts a
 * global member table, an owner, or a privilege the creator would keep.
 * Scenarios that need a second node's record go through real HTTP on ephemeral
 * loopback ports; discovery is driven against reachable, absent, left,
 * malformed and offline candidates.
 */
import { FetchTransport, MemoryStorage, OnlyneError, OnlyneNode } from '../src/index.js';
import type { Action, Auth, MsgInput, Mutation, NodeOptions, Plugins, Target } from '../src/types.js';

const clock = { value: Date.now() };
const urls: Record<string, string> = {};
const servers: Array<{ stop(closeActiveConnections?: boolean): void }> = [];
const running: Record<string, { stop(closeActiveConnections?: boolean): void }> = {};

function future(ms = 60_000): string { return new Date(clock.value + ms).toISOString(); }
function mut(): Mutation { return { idempotency_key: crypto.randomUUID(), retry_until: future() }; }
function keyed(idempotency_key: string): Mutation { return { idempotency_key, retry_until: future() }; }

function text(error: unknown): string {
  if (error instanceof OnlyneError) return `${error.error.code}: ${error.error.message}`;
  return error instanceof Error ? error.message : String(error);
}

/** Expects the call to be refused with exactly this code. */
async function refusal(code: string, run: () => Promise<unknown>): Promise<string> {
  try { await run(); return `expected ${code}, got success`; } catch (error) {
    const got = error instanceof OnlyneError ? error.error.code : `not-an-onlyne-error(${text(error)})`;
    return got === code ? '' : `expected ${code}, got ${got}`;
  }
}
const failures: string[] = [];
function check(name: string, problem: string): void {
  if (problem !== '') { failures.push(`${name}: ${problem}`); console.log(`FAIL ${name}: ${problem}`); return; }
  console.log(`ok   ${name}`);
}
function describeProblem(value: unknown): string {
  try { return JSON.stringify(value); } catch { return String(value); }
}
function keysOf(value: unknown): string[] {
  if (typeof value !== 'object' || value === null) return [];
  return Object.keys(value).sort();
}
function sameList(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((item, index) => item === b[index]);
}
function objectOf(value: unknown): Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) throw new Error(`Expected a JSON object, got ${describeProblem(value)}.`);
  const copy: Record<string, unknown> = {};
  for (const [name, item] of Object.entries(value)) copy[name] = item;
  return copy;
}
interface WireMembership { domain_id: string; node_id: string; state: string; revision: string }
function membershipOf(value: unknown): WireMembership {
  const fields = objectOf(value);
  const domain_id = fields['domain_id'];
  const node_id = fields['node_id'];
  const state = fields['state'];
  const revision = fields['revision'];
  if (typeof domain_id !== 'string' || typeof node_id !== 'string' || typeof state !== 'string' || typeof revision !== 'string') {
    throw new Error(`Expected a membership record, got ${describeProblem(value)}.`);
  }
  return { domain_id, node_id, state, revision };
}
function itemsOf(value: unknown): unknown[] {
  const items = objectOf(value)['items'];
  if (!Array.isArray(items)) throw new Error(`Expected a page with items, got ${describeProblem(value)}.`);
  return items;
}
function memberIds(value: unknown): string[] {
  return itemsOf(value).map(item => membershipOf(item).node_id);
}

interface Rig {
  node: OnlyneNode;
  /** Every SystemOne input this node was asked to decide on. */
  seen: MsgInput[];
  /** Queues the next decision this node's client returns. */
  deciding(action: Action): void;
  /** A second node object over the same plugins and the same Storage instance. */
  recreate(): OnlyneNode;
}

function rig(node_id: string, base_url: string, extra: Partial<Plugins> = {}): Rig {
  const storage = new MemoryStorage();
  const queue: Array<() => Promise<Action> | Action> = [];
  const seen: MsgInput[] = [];
  const queued = async (): Promise<Action> => {
    const next = queue.shift();
    if (next === undefined) throw new OnlyneError('decision_unavailable', 'No plugin decision.');
    return next();
  };
  const base: Plugins = {
    storage,
    resolve: async (node: string): Promise<Target> => {
      const endpoint = urls[node];
      if (endpoint === undefined) throw new OnlyneError('not_found', `No endpoint is configured for ${node}.`);
      return { node_id: node, base_url: endpoint, headers: { 'x-onlyne-actor': node_id } };
    },
    authenticate: async (request: Request): Promise<Auth | null> => {
      const actor = request.headers.get('x-onlyne-actor');
      return actor === null || actor === '' ? null : { actor };
    },
    decide: queued,
    ...extra,
  };
  const plugins: Plugins = {
    ...base,
    transport: base.transport ?? new FetchTransport(base.resolve),
    // The model input reaches the rig whether the decision is queued or supplied by the scenario.
    decide: async (input: MsgInput): Promise<Action> => {
      seen.push(input);
      return (extra.decide ?? queued)(input);
    },
  };
  const options: NodeOptions = { node_id, base_url, plugins, now: () => clock.value, id: () => crypto.randomUUID(), wait_ms: 5_000 };
  const node = new OnlyneNode(options);
  return {
    node,
    seen,
    deciding(action: Action) { queue.push(() => action); },
    recreate: () => new OnlyneNode({ ...options }),
  };
}

/** Reserves an ephemeral loopback port, then builds the node on the URL it will serve. */
function serve(node_id: string, extra: Partial<Plugins> = {}): Rig {
  let handler: (request: Request) => Promise<Response> = async () => new Response('starting', { status: 503 });
  const server = Bun.serve({ port: 0, hostname: '127.0.0.1', fetch: request => handler(request) });
  servers.push(server);
  running[node_id] = server;
  const base_url = `http://127.0.0.1:${server.port}/onlyne/v3`;
  urls[node_id] = base_url;
  const built = rig(node_id, base_url, extra);
  handler = request => built.node.fetch(request);
  return built;
}

/** A listener that is not an Onlyne node: it answers exactly as the scenario needs. */
function rogue(node_id: string, reply: (request: Request) => Response | Promise<Response>): string {
  const server = Bun.serve({ port: 0, hostname: '127.0.0.1', fetch: reply });
  servers.push(server);
  const base_url = `http://127.0.0.1:${server.port}/onlyne/v3`;
  urls[node_id] = base_url;
  return base_url;
}

function shutdown(): void {
  for (const server of servers) { try { server.stop(true); } catch { /* already stopped */ } }
}

interface Wire { status: number; body: unknown; text: string }
async function wire(node: string, method: string, path: string, options: { actor?: string; body?: unknown } = {}): Promise<Wire> {
  const base = urls[node];
  if (base === undefined) throw new Error(`No endpoint registered for ${node}.`);
  const headers: Record<string, string> = {};
  if (options.actor !== undefined) headers['x-onlyne-actor'] = options.actor;
  if (options.body !== undefined) headers['content-type'] = 'application/json';
  const response = await fetch(`${base}/${path.replace(/^\/+/, '')}`, {
    method,
    headers,
    body: options.body === undefined ? undefined : JSON.stringify(options.body),
  });
  const bodyText = await response.text();
  let body: unknown;
  try { body = bodyText === '' ? undefined : JSON.parse(bodyText); } catch { body = undefined; }
  return { status: response.status, body, text: bodyText };
}
function wireCode(answer: Wire): string {
  try {
    const error = objectOf(objectOf(answer.body)['error']);
    const code = error['code'];
    return typeof code === 'string' ? code : 'no-code';
  } catch { return 'no-error-record'; }
}

async function phase(name: string, run: () => Promise<void>): Promise<void> {
  try { await run(); } catch (error) { check(name, `unexpected error: ${text(error)}`); }
}

process.on('unhandledRejection', error => {
  const problem = `unhandled rejection: ${text(error)}`;
  failures.push(problem);
  console.log(`FAIL ${problem}`);
});
process.on('uncaughtException', error => {
  const problem = `uncaught exception: ${text(error)}`;
  failures.push(problem);
  console.log(`FAIL ${problem}`);
  shutdown();
  process.exit(1);
});

// =============================================================================
// 1. create: an absolute URI, a local-only record, no owner, no privilege
// =============================================================================

const createOwner = 'urn:onlyne:agent:create-owner';
const createJoiner = 'urn:onlyne:agent:create-joiner';
const createStranger = 'urn:onlyne:agent:create-stranger';

async function phaseCreate(): Promise<void> {
  const owner = serve(createOwner);
  const joiner = serve(createJoiner);
  const stranger = serve(createStranger);
  const ownerAuth: Auth = { actor: createOwner };
  const joinerAuth: Auth = { actor: createJoiner };
  const strangerAuth: Auth = { actor: createStranger };

  const created = await owner.node.domains.create(ownerAuth, mut());
  check('create generates an absolute urn:onlyne:domain URI',
    /^urn:onlyne:domain:[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(created.domain_id)
      ? '' : `got ${created.domain_id}`);
  check('create records the local node as active with a revision',
    created.node_id === createOwner && created.state === 'active' && created.revision.length > 0
      ? '' : describeProblem(created));
  check('a membership carries no owner field',
    sameList(keysOf(created), ['domain_id', 'node_id', 'revision', 'state']) ? '' : `keys ${keysOf(created).join(',')}`);
  check('create joins the creator locally in the same call',
    (await owner.node.domains.describe(ownerAuth, created.domain_id)).state === 'active' ? '' : 'the creator has no active record');
  check('create appears in the local active list',
    (await owner.node.domains.list(ownerAuth)).items.some(item => item.domain_id === created.domain_id) ? '' : 'missing from list');
  check('a node with an active membership cannot create a second domain',
    await refusal('conflict', () => owner.node.domains.create(ownerAuth, mut())));

  // No owner and no approval: the creator's endpoint is stopped before the other
  // node joins, so a join that needed the creator would fail here.
  running[createOwner]?.stop(true);
  const joined = await joiner.node.domains.join(joinerAuth, created.domain_id, mut());
  check('join needs no approval and grants the creator no privilege',
    joined.state === 'active' && joined.node_id === createJoiner && joined.domain_id === created.domain_id
      ? '' : describeProblem(joined));

  check('a foreign actor cannot join on this node',
    await refusal('forbidden', () => joiner.node.domains.join(ownerAuth, created.domain_id, mut())));
  check('a foreign actor cannot leave on this node',
    await refusal('forbidden', () => joiner.node.domains.leave(ownerAuth, created.domain_id, mut())));
  check('a foreign actor cannot create a membership on this node',
    await refusal('forbidden', () => joiner.node.domains.create(ownerAuth, mut())));
  check('reads require a non-empty actor',
    (await refusal('unauthenticated', () => owner.node.domains.list({ actor: '' })))
    || (await refusal('unauthenticated', () => owner.node.domains.describe({ actor: '' }, created.domain_id))));
  check('a fresh join rejects a supplied revision',
    await refusal('revision_conflict', () => stranger.node.domains.join(strangerAuth, created.domain_id, { ...mut(), expected_revision: 'not-a-record' })));
  check('an invalid domain id is refused',
    (await refusal('invalid_request', () => stranger.node.domains.join(strangerAuth, 'not a domain uri', mut())))
    || (await refusal('invalid_request', () => stranger.node.domains.join(strangerAuth, '', mut()))));
}

// =============================================================================
// 2. two nodes, one domain; revision fencing and the tombstone
// =============================================================================

const alpha = 'urn:onlyne:agent:alpha';
const beta = 'urn:onlyne:agent:beta';

async function phaseLifecycle(): Promise<void> {
  const alphaRig = serve(alpha);
  const betaRig = serve(beta);
  const gamma = 'urn:onlyne:agent:gamma';
  const gammaRig = serve(gamma);
  const alphaAuth: Auth = { actor: alpha };
  const betaAuth: Auth = { actor: beta };
  const gammaAuth: Auth = { actor: gamma };

  const created = await alphaRig.node.domains.create(alphaAuth, mut());
  const domain = created.domain_id;
  const joined = await betaRig.node.domains.join(betaAuth, domain, mut());

  check('two nodes persist their own record for one domain',
    joined.node_id === beta && joined.state === 'active' && joined.domain_id === domain
      ? '' : describeProblem(joined));
  check('each node lists only its own memberships, not a global table',
    (await alphaRig.node.domains.list(alphaAuth)).items.length === 1
      && (await betaRig.node.domains.list(betaAuth)).items.length === 1
      && (await alphaRig.node.domains.list(alphaAuth)).items[0]?.node_id === alpha
      ? '' : 'list is not node-local');
  check('a node describes its own record after joining',
    (await betaRig.node.domains.describe(betaAuth, domain)).revision === joined.revision ? '' : 'describe is not the joined record');

  check('an existing record needs expected_revision on join',
    await refusal('revision_conflict', () => betaRig.node.domains.join(betaAuth, domain, mut())));
  check('an existing record needs expected_revision on leave',
    await refusal('revision_conflict', () => betaRig.node.domains.leave(betaAuth, domain, mut())));
  check('a stale expected_revision is refused on join',
    await refusal('revision_conflict', () => betaRig.node.domains.join(betaAuth, domain, { ...mut(), expected_revision: 'stale-revision' })));
  check('a stale expected_revision is refused on leave',
    await refusal('revision_conflict', () => betaRig.node.domains.leave(betaAuth, domain, { ...mut(), expected_revision: 'stale-revision' })));

  const unchanged = await betaRig.node.domains.join(betaAuth, domain, { ...mut(), expected_revision: joined.revision });
  check('joining while already active with the current revision issues no new revision',
    unchanged.state === 'active' && unchanged.revision === joined.revision ? '' : describeProblem(unchanged));

  const left = await betaRig.node.domains.leave(betaAuth, domain, { ...mut(), expected_revision: joined.revision });
  check('leave marks the local record left with a new revision',
    left.state === 'left' && left.domain_id === domain && left.revision !== joined.revision ? '' : describeProblem(left));
  check('the left tombstone remains describable',
    (await betaRig.node.domains.describe(betaAuth, domain)).state === 'left' ? '' : 'describe lost the record');
  check('a left membership leaves the active list',
    (await betaRig.node.domains.list(betaAuth)).items.length === 0 ? '' : 'left record is still listed');

  check('a delayed rejoin carrying the pre-leave revision is refused',
    await refusal('revision_conflict', () => betaRig.node.domains.join(betaAuth, domain, { ...mut(), expected_revision: joined.revision })));
  check('leaving an absent membership is not_found',
    await refusal('not_found', () => betaRig.node.domains.leave(betaAuth, 'urn:onlyne:domain:never-joined', mut())));

  const rejoined = await betaRig.node.domains.join(betaAuth, domain, { ...mut(), expected_revision: left.revision });
  check('a rejoin against the left revision becomes active again',
    rejoined.state === 'active' && rejoined.revision !== left.revision ? '' : describeProblem(rejoined));

  const repeatedLeave = await betaRig.node.domains.leave(betaAuth, domain, { ...mut(), expected_revision: rejoined.revision });
  check('leaving with the current revision moves to left once',
    repeatedLeave.state === 'left' && repeatedLeave.revision !== rejoined.revision ? '' : describeProblem(repeatedLeave));
  const secondLeave = await betaRig.node.domains.leave(betaAuth, domain, { ...mut(), expected_revision: repeatedLeave.revision });
  check('leaving a left record with its current revision issues no new revision',
    secondLeave.state === 'left' && secondLeave.revision === repeatedLeave.revision ? '' : describeProblem(secondLeave));

  // One node, one active domain: a second domain waits until the first is left.
  const beforeOther = await betaRig.node.domains.join(betaAuth, domain, { ...mut(), expected_revision: secondLeave.revision });
  check('a rejoin after the left record settles becomes active',
    beforeOther.state === 'active' && beforeOther.revision !== secondLeave.revision ? '' : describeProblem(beforeOther));
  const other = await gammaRig.node.domains.create(gammaAuth, mut());
  check('joining a second domain while a membership is active is refused as conflict',
    await refusal('conflict', () => betaRig.node.domains.join(betaAuth, other.domain_id, mut())));
  check('the refused join records nothing for the second domain',
    await refusal('not_found', () => betaRig.node.domains.describe(betaAuth, other.domain_id)));

  const released = await betaRig.node.domains.leave(betaAuth, domain, { ...mut(), expected_revision: beforeOther.revision });
  check('leaving the active domain frees the node for another',
    released.state === 'left' && released.revision !== beforeOther.revision ? '' : describeProblem(released));
  const second = await betaRig.node.domains.join(betaAuth, other.domain_id, mut());
  const betaDomains = await betaRig.node.domains.list(betaAuth, { page_size: 10 });
  check('a node holds exactly one active membership after moving domains',
    second.state === 'active' && second.domain_id === other.domain_id
      && betaDomains.items.length === 1 && betaDomains.items[0]?.domain_id === other.domain_id
      ? '' : `list ${describeProblem(betaDomains)}`);
  check('the domain the node moved from is a left tombstone',
    (await betaRig.node.domains.describe(betaAuth, domain)).state === 'left' ? '' : 'the previous membership did not become left');
}

// =============================================================================
// 3. idempotent replay keeps history without reviving a closed membership
// =============================================================================

async function phaseReplay(): Promise<void> {
  const alphaRig = serve('urn:onlyne:agent:replay-alpha');
  const betaRig = serve('urn:onlyne:agent:replay-beta');
  const alphaAuth: Auth = { actor: 'urn:onlyne:agent:replay-alpha' };
  const betaAuth: Auth = { actor: 'urn:onlyne:agent:replay-beta' };

  const createKey = keyed('replay-create');
  const created = await alphaRig.node.domains.create(alphaAuth, createKey);
  const replayedCreate = await alphaRig.node.domains.create(alphaAuth, createKey);
  check('replaying create returns the original membership',
    replayedCreate.domain_id === created.domain_id && replayedCreate.revision === created.revision ? '' : describeProblem(replayedCreate));
  check('replaying create does not allocate a second domain',
    (await alphaRig.node.domains.list(alphaAuth)).items.length === 1 ? '' : 'the replay created a second record');

  const domain = created.domain_id;
  const joinKey = keyed('replay-join');
  const joined = await betaRig.node.domains.join(betaAuth, domain, joinKey);
  const leaveKey = keyed('replay-leave');
  const left = await betaRig.node.domains.leave(betaAuth, domain, { ...leaveKey, expected_revision: joined.revision });

  const replayedJoin = await betaRig.node.domains.join(betaAuth, domain, joinKey);
  const afterJoinReplay = await betaRig.node.domains.describe(betaAuth, domain);
  check('replaying join after leave returns the original historical record',
    replayedJoin.state === 'active' && replayedJoin.revision === joined.revision ? '' : describeProblem(replayedJoin));
  check('the historical join replay does not revive the membership',
    afterJoinReplay.state === 'left' && afterJoinReplay.revision === left.revision ? '' : describeProblem(afterJoinReplay));
  check('the historical join replay stays out of the active list',
    (await betaRig.node.domains.list(betaAuth)).items.length === 0 ? '' : 'the replay revived the listing');

  const again = await betaRig.node.domains.join(betaAuth, domain, { ...mut(), expected_revision: left.revision });
  const replayedLeave = await betaRig.node.domains.leave(betaAuth, domain, { ...leaveKey, expected_revision: joined.revision });
  const afterLeaveReplay = await betaRig.node.domains.describe(betaAuth, domain);
  check('replaying leave after a rejoin returns the historical left record',
    replayedLeave.state === 'left' && replayedLeave.revision === left.revision ? '' : describeProblem(replayedLeave));
  check('the historical leave replay does not close the rejoined membership',
    afterLeaveReplay.state === 'active' && afterLeaveReplay.revision === again.revision ? '' : describeProblem(afterLeaveReplay));
}

// =============================================================================
// 4. storage reconstruction keeps both tombstone and active record
// =============================================================================

async function phaseStorage(): Promise<void> {
  const nodeId = 'urn:onlyne:agent:storage-node';
  const first = serve(nodeId);
  const auth: Auth = { actor: nodeId };
  const leftDomain = (await first.node.domains.create(auth, mut())).domain_id;
  const leftRecord = await first.node.domains.leave(auth, leftDomain, { ...mut(), expected_revision: (await first.node.domains.describe(auth, leftDomain)).revision });
  const activeRecord = await first.node.domains.create(auth, mut());

  const rebuilt = first.recreate();
  const readLeft = await rebuilt.domains.describe(auth, leftDomain);
  const readActive = await rebuilt.domains.describe(auth, activeRecord.domain_id);
  const listed = await rebuilt.domains.list(auth);
  check('reconstruction keeps the left tombstone and its revision',
    readLeft.state === 'left' && readLeft.revision === leftRecord.revision ? '' : describeProblem(readLeft));
  check('reconstruction keeps the active record and its revision',
    readActive.state === 'active' && readActive.revision === activeRecord.revision ? '' : describeProblem(readActive));
  check('reconstruction lists only the active membership',
    listed.items.length === 1 && listed.items[0]?.domain_id === activeRecord.domain_id ? '' : describeProblem(listed));
}

// =============================================================================
// 5. active list paging
// =============================================================================

async function phasePaging(): Promise<void> {
  const nodeId = 'urn:onlyne:agent:paging-node';
  const pager = serve(nodeId);
  const auth: Auth = { actor: nodeId };
  const leftDomain = (await pager.node.domains.create(auth, mut())).domain_id;
  await pager.node.domains.leave(auth, leftDomain, { ...mut(), expected_revision: (await pager.node.domains.describe(auth, leftDomain)).revision });
  const active = (await pager.node.domains.create(auth, mut())).domain_id;

  const first = await pager.node.domains.list(auth, { page_size: 1 });
  check('a node with one active membership pages as a single item',
    first.items.length === 1 && first.items[0]?.domain_id === active ? '' : describeProblem(first));
  check('the single page is final and carries no next cursor',
    first.next_cursor === null ? '' : `next_cursor ${String(first.next_cursor)}`);
  check('a left membership is absent from the page',
    !first.items.some(item => item.domain_id === leftDomain) ? '' : 'the left domain leaked into paging');
}

// =============================================================================
// 6. discovery: candidates, dedup, filtering, degraded probes
// =============================================================================

const da = 'urn:onlyne:agent:discover-a';
const db = 'urn:onlyne:agent:discover-b';
const dc = 'urn:onlyne:agent:discover-c';
const outsider = 'urn:onlyne:agent:discover-outsider';
const rogueIdentity = 'urn:onlyne:agent:discover-rogue-identity';
const rogueDomain = 'urn:onlyne:agent:discover-rogue-domain';
const rogueGarbage = 'urn:onlyne:agent:discover-rogue-garbage';
const offline = 'urn:onlyne:agent:discover-offline';
const noCandidates = 'urn:onlyne:agent:discover-no-candidates';

interface CandidatePlan { domain: string; candidates: string[] }

async function phaseDiscover(): Promise<void> {
  const plan: CandidatePlan = { domain: '', candidates: [] };
  const discoveryA = serve(da, { domainCandidates: async (domain_id: string) => domain_id === plan.domain ? plan.candidates : [] });
  const discoveryB = serve(db, {
    domainCandidates: async (domain_id: string) => domain_id === plan.domain ? [da] : [],
  });
  const discoveryC = serve(dc);
  const outsiderRig = serve(outsider);
  const noCandidateRig = serve(noCandidates);
  const authA: Auth = { actor: da };
  const authB: Auth = { actor: db };
  const authC: Auth = { actor: dc };
  const authOutsider: Auth = { actor: outsider };
  const authNoCandidates: Auth = { actor: noCandidates };

  const created = await discoveryA.node.domains.create(authA, mut());
  plan.domain = created.domain_id;
  await discoveryB.node.domains.join(authB, plan.domain, mut());
  await discoveryC.node.domains.join(authC, plan.domain, mut());
  await outsiderRig.node.domains.create(authOutsider, mut());

  rogue(rogueIdentity, async () => new Response(JSON.stringify({ domain_id: plan.domain, node_id: `${rogueIdentity}-impostor`, state: 'active', revision: 'x' }), { status: 200, headers: { 'content-type': 'application/json' } }));
  rogue(rogueDomain, async () => new Response(JSON.stringify({ domain_id: 'urn:onlyne:domain:elsewhere', node_id: rogueDomain, state: 'active', revision: 'x' }), { status: 200, headers: { 'content-type': 'application/json' } }));
  rogue(rogueGarbage, async () => new Response('not-json', { status: 200, headers: { 'content-type': 'text/plain' } }));
  urls[offline] = 'http://127.0.0.1:1/onlyne/v3';
  plan.candidates = [db, dc, db, outsider, rogueIdentity, rogueDomain, rogueGarbage, offline, da];

  const discovered = await discoveryA.node.domains.discover(authA, plan.domain);
  check('discovery includes the local record even when candidates repeat it',
    sameList(discovered.members.map(member => member.node_id).sort(), [da, db, dc].sort()) ? '' : describeProblem(discovered.members.map(member => member.node_id)));
  check('discovery dedups repeated candidates',
    discovered.members.length === new Set(discovered.members.map(member => member.node_id)).size ? '' : 'a candidate appeared twice');
  check('discovery keeps only active records of this exact domain',
    discovered.members.every(member => member.state === 'active' && member.domain_id === plan.domain) ? '' : describeProblem(discovered.members));
  check('a candidate without a record is excluded, not reported as a member',
    !discovered.members.some(member => member.node_id === outsider) ? '' : 'an outsider was reported as a member');
  check('a candidate without a record is not an unavailable node',
    !discovered.unavailable.includes(outsider) ? '' : 'a clean not_found was reported as unavailable');
  check('identity-mismatched, domain-mismatched, malformed and offline probes are unavailable',
    sameList([...discovered.unavailable].sort(), [rogueIdentity, rogueDomain, rogueGarbage, offline].sort()) ? '' : describeProblem(discovered.unavailable));
  check('unavailable candidates never become members',
    !discovered.members.some(member => [rogueIdentity, rogueDomain, rogueGarbage, offline].includes(member.node_id)) ? '' : 'a degraded probe was treated as a member');

  // A left remote record is excluded and is not a degraded probe.
  const cRecord = await discoveryC.node.domains.describe(authC, plan.domain);
  await discoveryC.node.domains.leave(authC, plan.domain, { ...mut(), expected_revision: cRecord.revision });
  const afterLeave = await discoveryA.node.domains.discover(authA, plan.domain);
  check('a remote left record is excluded on the next discovery',
    !afterLeave.members.some(member => member.node_id === dc) ? '' : 'a left node was still reported active');
  check('a remote left record is not reported as unavailable',
    !afterLeave.unavailable.includes(dc) ? '' : 'a clean left record was reported unavailable');
  check('discovery is recomputed from candidates, not a cached member table',
    sameList(afterLeave.members.map(member => member.node_id).sort(), [da, db].sort()) ? '' : describeProblem(afterLeave.members.map(member => member.node_id)));

  // A node that omits itself from candidates still discovers its own membership.
  const local = await discoveryB.node.domains.discover(authB, plan.domain);
  check('discovery adds the local record even when the candidate source omits it',
    local.members.some(member => member.node_id === db) && local.members.some(member => member.node_id === da)
      ? '' : describeProblem(local.members.map(member => member.node_id)));

  check('discover refuses an absent local record',
    await refusal('not_found', () => discoveryA.node.domains.discover(authA, 'urn:onlyne:domain:absent')));
  check('discover with no candidate source is explicitly unsupported',
    await refusal('unsupported_delivery', async () => {
      const domain = (await noCandidateRig.node.domains.create(authNoCandidates, mut())).domain_id;
      await noCandidateRig.node.domains.discover(authNoCandidates, domain);
    }));
  check('discover requires a non-empty actor',
    await refusal('unauthenticated', () => discoveryA.node.domains.discover({ actor: '' }, plan.domain)));

  const reason = new Error('discovery cancelled by the caller');
  const controller = new AbortController();
  controller.abort(reason);
  let cancelled: unknown;
  let resolved = false;
  try {
    await discoveryA.node.domains.discover(authA, plan.domain, controller.signal);
    resolved = true;
  } catch (error) { cancelled = error; }
  check('a cancelled discover rejects with the caller reason and no member list',
    !resolved && (cancelled === reason || (cancelled instanceof Error && cancelled.name === 'AbortError'))
      ? '' : resolved ? 'discover resolved after cancellation' : text(cancelled));

  const ownerRecord = await discoveryA.node.domains.describe(authA, plan.domain);
  await discoveryA.node.domains.leave(authA, plan.domain, { ...mut(), expected_revision: ownerRecord.revision });
  check('discover refuses a left local membership',
    await refusal('not_a_member', () => discoveryA.node.domains.discover(authA, plan.domain)));
}

// =============================================================================
// 7. real HTTP: create, join, leave, describe, list, description
// =============================================================================

const httpA = 'urn:onlyne:agent:http-a';
const httpB = 'urn:onlyne:agent:http-b';
const httpC = 'urn:onlyne:agent:http-c';
const httpD = 'urn:onlyne:agent:http-d';
const httpE = 'urn:onlyne:agent:http-e';

async function phaseHttp(): Promise<void> {
  serve(httpA);
  serve(httpB);
  serve(httpC);
  const createBody = { idempotency_key: 'http-create', retry_until: future() };
  const created = await wire(httpA, 'POST', 'domains', { actor: httpA, body: createBody });
  check('HTTP create answers 201 with a membership',
    created.status === 201 && membershipOf(created.body).node_id === httpA
      ? '' : `status ${created.status} ${created.text}`);
  const domain = membershipOf(created.body).domain_id;
  check('HTTP create leaks no owner field',
    sameList(keysOf(created.body), ['domain_id', 'node_id', 'revision', 'state']) ? '' : `keys ${keysOf(created.body).join(',')}`);

  const replayed = await wire(httpA, 'POST', 'domains', { actor: httpA, body: createBody });
  check('HTTP create replay returns the same record',
    (replayed.status === 201 || replayed.status === 200)
      && membershipOf(replayed.body).domain_id === domain && membershipOf(replayed.body).revision === membershipOf(created.body).revision
      ? '' : describeProblem(replayed.body));
  const listA = await wire(httpA, 'GET', 'domains', { actor: httpA });
  check('HTTP list serves the local active memberships',
    listA.status === 200 && itemsOf(listA.body).length === 1 && memberIds(listA.body)[0] === httpA
      ? '' : `status ${listA.status} ${listA.text}`);

  const encoded = encodeURIComponent(domain);
  const joinBody = { idempotency_key: 'http-join', retry_until: future() };
  const joined = await wire(httpB, 'POST', `domains/${encoded}/join`, { actor: httpB, body: joinBody });
  check('HTTP join records the authenticated caller',
    joined.status === 200 && membershipOf(joined.body).node_id === httpB && membershipOf(joined.body).state === 'active'
      ? '' : `status ${joined.status} ${joined.text}`);
  check('HTTP join rejects a body carrying another actor identity',
    (await wire(httpB, 'POST', `domains/${encoded}/join`, { actor: httpA, body: { idempotency_key: 'http-foreign', retry_until: future() } })).status === 403
      ? '' : 'a foreign actor was not refused');

  const describedB = await wire(httpB, 'GET', `domains/${encoded}`, { actor: httpB });
  check('HTTP describe serves the caller node record',
    describedB.status === 200 && membershipOf(describedB.body).node_id === httpB ? '' : `status ${describedB.status} ${describedB.text}`);
  const describedC = await wire(httpC, 'GET', `domains/${encoded}`, { actor: httpC });
  check('HTTP describe of an absent record is not_found',
    describedC.status === 404 && wireCode(describedC) === 'not_found' ? '' : `status ${describedC.status} ${describedC.text}`);

  const leaveBody = { idempotency_key: 'http-leave', retry_until: future(), expected_revision: membershipOf(joined.body).revision };
  const left = await wire(httpB, 'POST', `domains/${encoded}/leave`, { actor: httpB, body: leaveBody });
  check('HTTP leave moves the caller record to left',
    left.status === 200 && membershipOf(left.body).state === 'left' ? '' : `status ${left.status} ${left.text}`);
  const describedAfter = await wire(httpB, 'GET', `domains/${encoded}`, { actor: httpB });
  check('HTTP describe still serves the left tombstone',
    membershipOf(describedAfter.body).state === 'left' ? '' : describeProblem(describedAfter.body));
  check('HTTP list drops the left membership',
    itemsOf((await wire(httpB, 'GET', 'domains', { actor: httpB })).body).length === 0 ? '' : 'left record is still listed');
  check('HTTP leave without expected_revision on a record is refused',
    (await wire(httpB, 'POST', `domains/${encoded}/leave`, { actor: httpB, body: { idempotency_key: 'http-leave-again', retry_until: future() } })).status === 409
      ? '' : 'a revision-less leave was accepted');

  const anonymous = await wire(httpA, 'GET', 'description');
  const anonymousFields = objectOf(anonymous.body);
  check('an unauthenticated description carries no domain fields',
    !('domains' in anonymousFields) && !('domain' in anonymousFields) && !('ingress' in anonymousFields)
      ? '' : `fields ${keysOf(anonymous.body).join(',')}`);
  check('an unauthenticated description names no domain id',
    !anonymous.text.includes(domain) ? '' : 'the description leaked a domain id');

  const authenticated = await wire(httpA, 'GET', 'description', { actor: httpA });
  const authenticatedFields = objectOf(authenticated.body);
  check('an authenticated description reports the local active domain as one id',
    authenticatedFields['domain'] === domain ? '' : describeProblem(authenticatedFields['domain']));
  check('an authenticated description carries no domains array',
    !('domains' in authenticatedFields) ? '' : `fields ${keysOf(authenticated.body).join(',')}`);
  const features = authenticatedFields['features'];
  check('an authenticated description advertises the domains feature',
    Array.isArray(features) && features.includes('domains') ? '' : describeProblem(features));
  check('an authenticated description keeps no ingress claim',
    !('ingress' in authenticatedFields) ? '' : `fields ${keysOf(authenticated.body).join(',')}`);

  const noMembership = objectOf((await wire(httpC, 'GET', 'description', { actor: httpC })).body);
  check('an authenticated description without a membership reports a null domain',
    noMembership['domain'] === null && !('domains' in noMembership) && !('ingress' in noMembership)
      ? '' : `fields ${keysOf(noMembership).join(',')}`);

  const unauthorized = await wire(httpA, 'POST', 'domains', { body: { idempotency_key: 'http-anon', retry_until: future() } });
  check('HTTP create without a credential is unauthenticated',
    unauthorized.status === 401 && wireCode(unauthorized) === 'unauthenticated' ? '' : `status ${unauthorized.status} ${unauthorized.text}`);

  const httpPlan = { domain: '' };
  const discoveryHost = serve(httpD, { domainCandidates: async (domain_id: string) => domain_id === httpPlan.domain ? [httpE] : [] });
  serve(httpE);
  httpPlan.domain = (await discoveryHost.node.domains.create({ actor: httpD }, mut())).domain_id;
  await wire(httpE, 'POST', `domains/${encodeURIComponent(httpPlan.domain)}/join`, {
    actor: httpE,
    body: { idempotency_key: 'http-discovery-join', retry_until: future() },
  });
  const membersRoute = await wire(httpD, 'GET', `domains/${encodeURIComponent(httpPlan.domain)}/members`, { actor: httpD });
  const membersBody = objectOf(membersRoute.body);
  const remoteMembers = membersBody['members'];
  check('HTTP members route answers the discovery result',
    membersRoute.status === 200 && Array.isArray(remoteMembers) && membersBody['domain_id'] === httpPlan.domain
      ? '' : `status ${membersRoute.status} ${membersRoute.text}`);
  check('HTTP discovery includes the local node and the reachable candidate',
    Array.isArray(remoteMembers)
      && sameList(remoteMembers.map(item => membershipOf(item).node_id).sort(), [httpD, httpE].sort())
      ? '' : describeProblem(remoteMembers));
  check('HTTP discovery reports no unavailable node when every candidate answers',
    Array.isArray(membersBody['unavailable']) && membersBody['unavailable'].length === 0 ? '' : describeProblem(membersBody['unavailable']));
}

// =============================================================================
// 8. the model entry: a domain action chosen by SystemOne
// =============================================================================

async function phaseClient(): Promise<void> {
  const nodeId = 'urn:onlyne:agent:client-domain';
  const clientRig = serve(nodeId);
  const auth: Auth = { actor: nodeId };
  const created = await clientRig.node.domains.create(auth, mut());

  clientRig.deciding({ kind: 'domain', op: 'list' });
  const listed = await clientRig.node.client.msg({ delivery_kind: 'domain', msg: 'list my domains' });
  check('client.msg routes a decided list to the domain service',
    memberIds(listed).length === 1 && memberIds(listed)[0] === nodeId ? '' : describeProblem(listed));
  check('the decided action carries the model input and message',
    clientRig.seen.length === 1 && clientRig.seen[0]?.delivery_kind === 'domain' && clientRig.seen[0]?.msg === 'list my domains'
      ? '' : describeProblem(clientRig.seen));

  const otherOwner = serve('urn:onlyne:agent:client-domain-other');
  const otherAuth: Auth = { actor: 'urn:onlyne:agent:client-domain-other' };
  const third = await otherOwner.node.domains.create(otherAuth, mut());
  clientRig.deciding({ kind: 'domain', op: 'join', domain_id: third.domain_id });
  check('a decided join into a second domain while active is refused as conflict',
    await refusal('conflict', () => clientRig.node.client.msg({ delivery_kind: 'domain', msg: 'join the other cluster domain' })));

  clientRig.deciding({ kind: 'domain', op: 'leave', domain_id: created.domain_id, expected_revision: created.revision });
  const leftByModel = membershipOf(await clientRig.node.client.msg({ delivery_kind: 'domain', msg: 'leave my current domain' }));
  check('client.msg routes a decided leave of the active domain',
    leftByModel.domain_id === created.domain_id && leftByModel.state === 'left' ? '' : describeProblem(leftByModel));

  clientRig.deciding({ kind: 'domain', op: 'join', domain_id: third.domain_id });
  const joinedByModel = await clientRig.node.client.msg({ delivery_kind: 'domain', msg: 'join the other cluster domain' });
  const joinedBody = membershipOf(joinedByModel);
  check('a decided join after the leave moves the node into the other domain',
    joinedBody.domain_id === third.domain_id && joinedBody.node_id === nodeId && joinedBody.state === 'active'
      ? '' : describeProblem(joinedBody));
  check('the decided join is persisted locally',
    (await clientRig.node.domains.describe(auth, third.domain_id)).revision === joinedBody.revision ? '' : 'the join was not persisted');
  const clientDomains = await clientRig.node.domains.list(auth, { page_size: 10 });
  check('the client node lists one active domain after the decided move',
    clientDomains.items.length === 1 && clientDomains.items[0]?.domain_id === third.domain_id
      ? '' : describeProblem(clientDomains));
  check('the domain the client left survives as a tombstone',
    (await clientRig.node.domains.describe(auth, created.domain_id)).state === 'left' ? '' : 'the left domain was lost');

  clientRig.deciding({ kind: 'domain', op: 'describe', domain_id: third.domain_id });
  const described = membershipOf(await clientRig.node.client.msg({ delivery_kind: 'domain', msg: 'describe that domain' }));
  check('client.msg routes a decided describe',
    described.domain_id === third.domain_id && described.state === 'active' ? '' : describeProblem(described));
}

// =============================================================================
// 9. a domain membership grants no thread rights
// =============================================================================

const threadOwner = 'urn:onlyne:agent:thread-owner';
const threadGuest = 'urn:onlyne:agent:thread-guest';

async function phaseThreadRights(): Promise<void> {
  const ownerRig = serve(threadOwner);
  const guestRig = serve(threadGuest);
  const ownerAuth: Auth = { actor: threadOwner };
  const guestAuth: Auth = { actor: threadGuest };
  const domain = (await ownerRig.node.domains.create(ownerAuth, mut())).domain_id;
  const thread = await ownerRig.node.threads.create(ownerAuth, { ...mut(), title: 'owner work', default: true });

  const before = await ownerRig.node.threads.members(ownerAuth, thread.thread_ref);
  await guestRig.node.domains.join(guestAuth, domain, mut());
  const after = await ownerRig.node.threads.members(ownerAuth, thread.thread_ref);
  check('joining a domain mutates no thread membership list',
    sameList(before.items.map(member => member.peer), after.items.map(member => member.peer)) && after.items.length === before.items.length
      ? '' : describeProblem(after.items.map(member => member.peer)));
  check('a domain member is not a thread member',
    !after.items.some(member => member.peer === threadGuest) ? '' : 'the domain join added a thread member');

  const sendProblem = await refusal('not_a_member', () => ownerRig.node.messages.admit(guestAuth, {
    ...mut(),
    message: { id: crypto.randomUUID(), sender: threadGuest, delivery_kind: 'text', msg: 'hello from the same domain', meta: { thread: thread.thread_ref } },
    target_endpoint: threadOwner,
    plan: { response: 'none', execution: 'existing' },
  }));
  check('a domain member cannot post into the owner thread', sendProblem);
  check('a domain member cannot read the owner thread',
    await refusal('forbidden', () => ownerRig.node.threads.describe(guestAuth, thread.thread_ref)));
  check('creating and joining domains creates no thread on the guest',
    (await guestRig.node.threads.list(guestAuth)).items.length === 0 ? '' : 'the guest gained a thread');
}

// ---- run --------------------------------------------------------------------

try {
  await phase('create', phaseCreate);
  await phase('two nodes', phaseLifecycle);
  await phase('idempotent replay', phaseReplay);
  await phase('storage reconstruction', phaseStorage);
  await phase('paging', phasePaging);
  await phase('discovery', phaseDiscover);
  await phase('http', phaseHttp);
  await phase('client entries', phaseClient);
  await phase('thread rights', phaseThreadRights);
} finally {
  shutdown();
  console.log(failures.length === 0 ? '\n全部通过' : `\n${failures.length} 项失败`);
  if (failures.length > 0) process.exit(1);
}
