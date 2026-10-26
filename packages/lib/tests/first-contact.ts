/**
 * Runtime acceptance for owner-policy first contact.
 *
 * Run: bun tests/first-contact.ts
 *
 * A message with no explicit thread and no reply landing normally goes to the owner's
 * default thread. When the owner has none, the sender is still admitted if the owner's
 * inbound policy says so: the thread, the sender's membership and the grant notice
 * commit in the same transaction as the message and its receipt. Discovery of a
 * landing grants nothing, a refusal leaves no record behind, and an explicit thread
 * that does not exist never falls back to the default.
 */
import { MemoryStorage, OnlyneError, OnlyneNode } from '../src/index.js';
import type { Auth, DeliveryPlan, Mutation, NodeOptions, Plugins, SendRequest, Target } from '../src/types.js';

const ownerNode = 'urn:onlyne:agent:owner';
const guestNode = 'urn:onlyne:agent:guest';

const clock = { value: Date.now() };
const plan: DeliveryPlan = { response: 'async', execution: 'existing' };

function future(ms = 60_000): string { return new Date(clock.value + ms).toISOString(); }
function mut(): Mutation { return { idempotency_key: crypto.randomUUID(), retry_until: future() }; }

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

function rig(node_id: string, extra: Partial<Plugins> = {}): OnlyneNode {
  const plugins: Plugins = {
    storage: new MemoryStorage(),
    resolve: async (node: string): Promise<Target> => ({ node_id: node, base_url: 'http://127.0.0.1:1/onlyne/v3', headers: { 'x-onlyne-actor': node_id } }),
    authenticate: async (request: Request): Promise<Auth | null> => {
      const actor = request.headers.get('x-onlyne-actor');
      return actor === null || actor === '' ? null : { actor };
    },
    decide: async () => { throw new OnlyneError('decision_unavailable', 'No plugin decision.'); },
    ...extra,
  };
  const options: NodeOptions = { node_id, base_url: 'http://127.0.0.1:1/onlyne/v3', plugins, now: () => clock.value, id: () => crypto.randomUUID(), wait_ms: 1_000 };
  return new OnlyneNode(options);
}

async function phase(name: string, run: () => Promise<void>): Promise<void> {
  try { await run(); } catch (error) { check(name, `unexpected error: ${text(error)}`); }
}

process.on('unhandledRejection', error => {
  const problem = `unhandled rejection: ${text(error)}`;
  failures.push(problem);
  console.log(`FAIL ${problem}`);
});

// =============================================================================
// 1. the owner's policy admits the sender: one thread, one membership, one notice
// =============================================================================

async function phasePermitted(): Promise<void> {
  const calls: string[] = [];
  const owner = rig(ownerNode, { inboundThread: async (actor: string) => { calls.push(actor); return true; } });
  const ownerAuth: Auth = { actor: ownerNode };
  const guestAuth: Auth = { actor: guestNode };
  const request: SendRequest = {
    ...mut(),
    message: { id: 'first-01', sender: guestNode, delivery_kind: '请求协作', msg: '第一次联系，想核对实验结果。' },
    target_endpoint: ownerNode, plan,
  };

  const receipt = await owner.messages.admit(guestAuth, request);
  check('first-contact.admit', receipt.seq === '1' && receipt.thread_ref.owner === ownerNode && receipt.thread_ref.id.length > 0
    ? '' : `unexpected receipt ${describeProblem(receipt)}`);
  check('first-contact.policy', calls.length === 1 && calls[0] === guestNode ? '' : `inbound policy saw ${describeProblem(calls)}`);

  const described = await owner.threads.describe(ownerAuth, receipt.thread_ref);
  check('first-contact.thread_shape',
    sameList(keysOf(described.thread), ['closed_at', 'created_at', 'event_seq', 'next_seq', 'state', 'thread_ref', 'title'])
      ? '' : `thread keys ${keysOf(described.thread).join(',')}`);
  check('first-contact.thread_open',
    described.thread.state === 'open' && described.thread.next_seq === '2' && described.thread.event_seq === '2'
      ? '' : describeProblem(described.thread));

  const membership = described.members.find(member => member.peer === guestNode);
  check('first-contact.membership_shape',
    membership !== undefined && sameList(keysOf(membership), ['access', 'peer', 'read_scope', 'revision', 'state'])
      ? '' : `membership ${describeProblem(membership)}`);
  check('first-contact.membership_active',
    membership?.state === 'active' && sameList([...(membership?.access ?? [])].sort(), ['post', 'read'])
      ? '' : describeProblem(membership));

  // The notice the owner issued, read from the owner's outbox: `grants()` is the recipient's
  // accepted list, and this node is the owner here.
  const grants = await owner.threads.notices(ownerAuth);
  const notice = grants.items[0];
  check('first-contact.grant_notice',
    grants.items.length === 1 && notice?.thread_ref.id === receipt.thread_ref.id
      && notice?.revision === membership?.revision && notice?.acked === false
      ? '' : describeProblem(grants.items));
  check('first-contact.grant_shape',
    notice !== undefined && sameList(keysOf(notice), ['acked', 'id', 'recipient', 'revision', 'thread_ref'])
      ? '' : `notice keys ${keysOf(notice).join(',')}`);

  const inbox = await owner.messages.inbox(ownerAuth);
  check('first-contact.owner_attention',
    inbox.items.length === 1 && inbox.items[0]?.attention.recipient === ownerNode
      && inbox.items[0]?.attention.message_ref.id === 'first-01'
      ? '' : describeProblem(inbox.items));

  const events = await owner.messages.events(ownerAuth, receipt.thread_ref);
  check('first-contact.events', events.items.map(event => event.type).join(',') === 'membership,message'
    ? '' : `events ${events.items.map(event => event.type).join(',')}`);

  const again = await owner.messages.admit(guestAuth, request);
  check('first-contact.duplicate',
    again.receipt_id === receipt.receipt_id && again.thread_ref.id === receipt.thread_ref.id
      ? '' : `duplicate returned ${describeProblem(again)}`);
  check('first-contact.duplicate_no_reauth', calls.length === 1 ? '' : `the policy ran ${calls.length} times`);
  check('first-contact.single_thread', (await owner.threads.list(ownerAuth)).items.length === 1
    ? '' : 'a duplicate opened a second thread');

  const followup = await owner.messages.admit(guestAuth, {
    ...mut(),
    message: { id: 'first-02', sender: guestNode, delivery_kind: '补充', msg: '再补一句。', meta: { thread: receipt.thread_ref } },
    target_endpoint: ownerNode, plan,
  });
  check('first-contact.member_can_post', followup.seq === '2' && calls.length === 1
    ? '' : `second message ${describeProblem(followup)} after ${calls.length} policy calls`);

  await owner.messages.admit(ownerAuth, {
    ...mut(),
    message: { id: 'owner-01', sender: ownerNode, delivery_kind: '答复', msg: '收到了。', meta: { thread: receipt.thread_ref } },
    target_endpoint: ownerNode, plan,
  });
  check('first-contact.no_broadcast_attention',
    (await owner.messages.attention(guestAuth, receipt.thread_ref)).items.length === 0
      ? '' : 'an unmentioned owner message became an obligation for the guest');
}

// =============================================================================
// 2. a refusal is recorded, grants nothing, and leaves no orphan records
// =============================================================================

async function phaseRefused(): Promise<void> {
  const ownerAuth: Auth = { actor: ownerNode };
  const guestAuth: Auth = { actor: guestNode };
  const refused = (id: string): SendRequest => ({
    ...mut(),
    message: { id, sender: guestNode, delivery_kind: '请求协作', msg: '没有默认 thread 也想说话。' },
    target_endpoint: ownerNode, plan,
  });

  let permit = false;
  const calls: string[] = [];
  const denied = rig(ownerNode, { inboundThread: async (actor: string) => { calls.push(actor); return permit; } });
  check('first-contact.policy_denied', await refusal('not_found', () => denied.messages.admit(guestAuth, refused('deny-01'))));

  const absent = rig(ownerNode);
  check('first-contact.policy_absent', await refusal('not_found', () => absent.messages.admit(guestAuth, refused('absent-01'))));

  const gated = rig(ownerNode, { inboundThread: async () => true, mayAdmit: async () => false });
  check('first-contact.admission_policy', await refusal('forbidden', () => gated.messages.admit(guestAuth, refused('gate-01'))));

  const empty: Array<readonly [string, OnlyneNode]> = [['denied', denied], ['absent', absent], ['gated', gated]];
  for (const [name, node] of empty) {
    const threads = await node.threads.list(ownerAuth);
    const notices = await node.threads.grants(guestAuth);
    const inbox = await node.messages.inbox(ownerAuth);
    check(`first-contact.refusal_leaves_nothing.${name}`,
      threads.items.length === 0 && notices.items.length === 0 && inbox.items.length === 0
        ? '' : `threads ${threads.items.length}, notices ${notices.items.length}, inbox ${inbox.items.length}`);
  }

  // The recorded refusal outlives a later policy change: same identity, same answer,
  // and the policy is not consulted a second time.
  permit = true;
  check('first-contact.refusal_recorded', await refusal('not_found', () => denied.messages.admit(guestAuth, refused('deny-01'))));
  check('first-contact.refusal_no_reauth', calls.length === 1 ? '' : `the policy ran ${calls.length} times for a recorded refusal`);

  const fresh = await denied.messages.admit(guestAuth, refused('deny-02'));
  check('first-contact.policy_applies_fresh', fresh.seq === '1' && calls.length === 2
    ? '' : `a fresh identity got ${describeProblem(fresh)} after ${calls.length} policy calls`);
}

// =============================================================================
// 3. an explicit thread never falls back; a default location never authorizes
// =============================================================================

async function phaseExplicit(): Promise<void> {
  const calls: string[] = [];
  const owner = rig(ownerNode, { inboundThread: async (actor: string) => { calls.push(actor); return true; } });
  const ownerAuth: Auth = { actor: ownerNode };
  const guestAuth: Auth = { actor: guestNode };
  const landing = await owner.threads.create(ownerAuth, { ...mut(), title: 'existing work', default: true });

  check('explicit-unknown.refused', await refusal('not_found', () => owner.messages.admit(guestAuth, {
    ...mut(),
    message: { id: 'explicit-01', sender: guestNode, delivery_kind: '投递', msg: '送到一个不存在的 thread。', meta: { thread: { owner: ownerNode, id: 'thr-missing' } } },
    target_endpoint: ownerNode, plan,
  })));
  check('explicit-unknown.no_policy', calls.length === 0 ? '' : `the inbound policy ran for an explicit thread: ${describeProblem(calls)}`);
  const kept = await owner.threads.describe(ownerAuth, landing.thread_ref);
  check('explicit-unknown.default_untouched',
    kept.thread.next_seq === landing.next_seq && kept.thread.event_seq === landing.event_seq
      ? '' : describeProblem(kept.thread));
  check('explicit-unknown.no_fallback_thread', (await owner.threads.list(ownerAuth)).items.length === 1
    ? '' : 'an explicit unknown thread created a fallback thread');

  check('default-location.grants_nothing', await refusal('not_a_member', () => owner.messages.admit(guestAuth, {
    ...mut(),
    message: { id: 'default-01', sender: guestNode, delivery_kind: '投递', msg: '没有 thread，但 owner 有默认位置。' },
    target_endpoint: ownerNode, plan,
  })));
  check('default-location.no_policy', calls.length === 0 ? '' : `the inbound policy ran while a default existed: ${describeProblem(calls)}`);
  const after = await owner.threads.describe(ownerAuth, landing.thread_ref);
  check('default-location.unchanged', after.thread.next_seq === landing.next_seq && after.members.length === 0
    ? '' : `default thread advanced to ${after.thread.next_seq} with ${after.members.length} members`);
}

// ---- run --------------------------------------------------------------------

try {
  await phase('permitted first contact', phasePermitted);
  await phase('refusals', phaseRefused);
  await phase('explicit threads', phaseExplicit);
} finally {
  console.log(failures.length === 0 ? '\n全部通过' : `\n${failures.length} 项失败`);
  if (failures.length > 0) process.exit(1);
}
