/**
 * Runtime acceptance for the Onlyne v3 library.
 *
 * Run: bun tests/verify.ts
 *
 * Every named check is a behavioural assertion against the public API. Storage in
 * these scenarios is MemoryStorage, whose guarantee is `binding-defined`: it is
 * process-local, so "recovery" below means a fresh node object over the same live
 * Storage instance, never a crash.
 */
import { CLOCK_SKEW_MS, EXCHANGE_RETRY_MS, REFUSED_RETRY_MS, FetchTransport, MemoryStorage, OnlyneError, OnlyneNode, generateKeyMaterial, trustPhase } from '../src/index.js';
import { canonical } from '../src/core.js';
import { createPrivateKey, sign } from 'node:crypto';
import type {
  Auth, DeliveryPlan, Entry, ErrorCode, KeyMaterial, Mutation, NodeOptions, Plugins, Resource, ResourceDriver,
  SendRequest, Target, TrustSeed,
} from '../src/types.js';

const lazuliNode = 'urn:onlyne:agent:lazuli';
const dpdpNode = 'urn:onlyne:agent:dpdp';
const caeruliaNode = 'urn:onlyne:agent:caerulia';
const keeperNode = 'urn:onlyne:agent:keeper';
const senderNode = 'urn:onlyne:agent:sender';
const receiverNode = 'urn:onlyne:agent:receiver';
const larchNode = 'urn:onlyne:agent:larch';
const mossNode = 'urn:onlyne:agent:moss';
const nettleNode = 'urn:onlyne:agent:nettle';
const thistleNode = 'urn:onlyne:agent:thistle';

/** One clock for every node in this script; only the expiry scenarios move it. */
const clock = { value: Date.now() };

/** Real HTTP endpoints of the nodes served by this script, filled as servers start. */
const urls: Record<string, string> = {};
const servers: Array<{ stop(closeActiveConnections?: boolean): void }> = [];

const plan: DeliveryPlan = { response: 'async', execution: 'existing' };
const awaitPlan: DeliveryPlan = { response: 'await', execution: 'existing' };

function future(ms = 60_000): string { return new Date(clock.value + ms).toISOString(); }
function mut(): Mutation { return { idempotency_key: crypto.randomUUID(), retry_until: future() }; }
const sleep = (ms: number): Promise<void> => new Promise<void>(resolve => setTimeout(resolve, ms));

function text(error: unknown): string {
  if (error instanceof OnlyneError) return `${error.error.code}: ${error.error.message}`;
  return error instanceof Error ? error.message : String(error);
}
function codeOf(error: unknown): string {
  return error instanceof OnlyneError ? error.error.code : `not-an-onlyne-error(${text(error)})`;
}

/** Expects the call to be refused with exactly this code. */
async function refusal(code: string, run: () => Promise<unknown>): Promise<string> {
  try { await run(); return `expected ${code}, got success`; } catch (error) {
    const got = codeOf(error);
    return got === code ? '' : `expected ${code}, got ${got}`;
  }
}

/** Expects the call to succeed. An expired obligation must still accept its ack. */
async function succeeds(run: () => Promise<unknown>): Promise<string> {
  try { await run(); return ''; } catch (error) { return `unexpected failure: ${text(error)}`; }
}

const failures: string[] = [];
function check(name: string, problem: string): void {
  if (problem !== '') { failures.push(`${name}: ${problem}`); console.log(`FAIL ${name}: ${problem}`); return; }
  console.log(`ok   ${name}`);
}

interface Shared { injected: Entry[]; allow: boolean }
/** Every `notify` event one rig saw, in arrival order. */
const notified: Array<{ node: string; event: unknown }> = [];

interface Rig {
  node: OnlyneNode;
  storage: MemoryStorage;
  shared: Shared;
  /** A second node object over the same plugins and the same Storage instance. */
  recreate(): OnlyneNode;
}

function rig(node_id: string, base_url: string, extra: Partial<Plugins> = {}): Rig {
  const storage = new MemoryStorage();
  const shared: Shared = { injected: [], allow: true };
  const base: Plugins = {
    storage,
    resolve: async (node: string): Promise<Target> => ({ node_id: node, base_url: urls[node] ?? base_url, headers: { 'x-onlyne-actor': node_id } }),
    authenticate: async (request: Request): Promise<Auth | null> => {
      const actor = request.headers.get('x-onlyne-actor');
      return actor === null || actor === '' ? null : { actor };
    },
    decide: async () => { throw new OnlyneError('decision_unavailable', 'No plugin decision.'); },
    mention: async alias => (alias === 'lazuli' ? lazuliNode : alias),
    runtime: {
      supports: async () => true,
      notify: event => notified.push({ node: node_id, event }),
      inject: async (entry: Entry, context?: string) => {
        shared.injected.push(entry);
        return { context: context ?? node_id, consumed: shared.allow };
      },
      todo: async () => {},
    },
    ...extra,
  };
  // The exported transport is what a real host injects; a caller-supplied one wins.
  const plugins: Plugins = { ...base, transport: base.transport ?? new FetchTransport(base.resolve) };
  const options: NodeOptions = { node_id, base_url, plugins, now: () => clock.value, id: () => crypto.randomUUID(), wait_ms: 5_000 };
  const node = new OnlyneNode(options);
  return {
    node,
    storage,
    shared,
    // A fresh node over the same plugins: only the plugins and the Storage instance carry over.
    recreate: () => new OnlyneNode({ ...options }),
  };
}

/** Reserves an ephemeral loopback port, then builds the node on the URL it will serve. */
function serve(node_id: string, make: (base_url: string) => Rig): Rig {
  let handler: (request: Request) => Promise<Response> = async () => new Response('starting', { status: 503 });
  const server = Bun.serve({ port: 0, hostname: '127.0.0.1', fetch: request => handler(request) });
  servers.push(server);
  const base_url = `http://127.0.0.1:${server.port}/onlyne/v3`;
  const rig = make(base_url);
  urls[node_id] = base_url;
  handler = request => rig.node.fetch(request);
  return rig;
}

async function waitFor(predicate: () => Promise<boolean>, timeoutMs = 5_000): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) { if (await predicate()) return true; await sleep(20); }
  return false;
}

/** Drives `pending` with repeated reconciliation until it settles, then returns its outcome. */
async function polling<T>(pending: Promise<T>, drive: () => Promise<unknown>, timeoutMs = 5_000): Promise<T> {
  const outcome: { value?: T; error?: unknown; done: boolean } = { done: false };
  // Attach both handlers now, so a rejection during the loop is never unhandled.
  pending.then(value => { outcome.value = value; outcome.done = true; }, error => { outcome.error = error; outcome.done = true; });
  const deadline = Date.now() + timeoutMs;
  while (!outcome.done && Date.now() < deadline) { await drive(); await sleep(20); }
  if (!outcome.done) return pending;
  if ('error' in outcome) throw outcome.error;
  return outcome.value as T;
}

/** SHA-256 hex of a request body — the RFC 0017 content digest. */
function digestHex(text: string): string {
  return Bun.CryptoHasher.hash('sha256', text, 'hex') as string;
}

function shutdown(): void {
  for (const server of servers) { try { server.stop(true); } catch { /* already stopped */ } }
}

process.on('unhandledRejection', error => {
  console.log(`FAIL unhandled rejection: ${text(error)}`);
  shutdown();
  process.exit(1);
});
process.on('uncaughtException', error => {
  console.log(`FAIL uncaught exception: ${text(error)}`);
  shutdown();
  process.exit(1);
});

// ---- fixtures ---------------------------------------------------------------

const lazuli = rig(lazuliNode, 'http://127.0.0.1:1/onlyne/v3', { mayAdmit: async () => true });
const expiring = rig(lazuliNode, 'http://127.0.0.1:9/onlyne/v3', { mayAdmit: async () => true });
const lazuliAuth: Auth = { actor: lazuliNode };
const dpdpAuth: Auth = { actor: dpdpNode };
const outsiderAuth: Auth = { actor: caeruliaNode };

// =============================================================================
// 1. local API: threads, admission, replay, obligations, permits, expiry
// =============================================================================
async function phaseLocal(): Promise<void> {
  check('storage.guarantee', lazuli.storage.guarantee === 'binding-defined'
    ? '' : `MemoryStorage declared ${lazuli.storage.guarantee}`);

  const thread = await lazuli.node.threads.create(lazuliAuth, { ...mut(), title: 'sea ice review' });
  check('thread.create', thread.state === 'open' && thread.next_seq === '1' ? '' : `unexpected thread ${JSON.stringify(thread)}`);

  const joined = await lazuli.node.threads.member(lazuliAuth, thread.thread_ref, dpdpNode, { access: ['post', 'read'] }, mut());
  check('thread.member', joined.state === 'active' && joined.access.includes('post') ? '' : 'membership did not activate');

  const members = await lazuli.node.threads.members(lazuliAuth, thread.thread_ref);
  check('thread.members', members.items.some(m => m.peer === dpdpNode && m.state === 'active') ? '' : 'the member table did not record dpdp');

  const sharedKey = crypto.randomUUID();
  const send: SendRequest = {
    idempotency_key: sharedKey, retry_until: future(),
    message: { id: 'msg-01', sender: dpdpNode, delivery_kind: '核对结论', msg: '请核对这次实验的结论。', meta: { thread: thread.thread_ref, mentions: ['@lazuli'] } },
    target_endpoint: lazuliNode, plan,
  };
  const receipt = await lazuli.node.messages.admit(dpdpAuth, send);
  check('messages.admit', receipt.seq === '1' && receipt.thread_ref.id === thread.thread_ref.id ? '' : `unexpected receipt ${JSON.stringify(receipt)}`);
  check('messages.admit.guarantee', receipt.guarantee === 'binding-defined' ? '' : `the receipt claimed ${receipt.guarantee}`);

  const again = await lazuli.node.messages.admit(dpdpAuth, send);
  check('messages.replay', again.receipt_id === receipt.receipt_id ? '' : 'a replay created a second admission');

  check('messages.conflict', await refusal('id_conflict', () => lazuli.node.messages.admit(dpdpAuth, {
    ...send, idempotency_key: crypto.randomUUID(),
    message: { ...send.message, msg: '改写过的正文' },
  })));
  const stable = await lazuli.node.messages.admit(dpdpAuth, send);
  check('messages.receipt.stable', stable.receipt_id === receipt.receipt_id
    ? '' : 'a refused rewrite replaced the original receipt');
  check('messages.key_conflict', await refusal('id_conflict', () => lazuli.node.messages.admit(dpdpAuth, {
    ...send, idempotency_key: sharedKey,
    message: { ...send.message, id: 'msg-02', msg: '另一个身份' },
  })));

  // A refused identity stays refused across a new node object over the same Storage,
  // and a later membership does not revive it.
  check('messages.refusal.not_member', await refusal('not_a_member', () => lazuli.node.messages.admit(outsiderAuth, {
    idempotency_key: crypto.randomUUID(), retry_until: future(),
    message: { id: 'msg-out-01', sender: caeruliaNode, delivery_kind: '插话', msg: '我也能说话吗。', meta: { thread: thread.thread_ref } },
    target_endpoint: lazuliNode, plan,
  })));
  const revived = lazuli.recreate();
  const admitted = await revived.threads.member(lazuliAuth, thread.thread_ref, caeruliaNode, { access: ['post', 'read'] }, mut());
  check('thread.member.later', admitted.state === 'active' ? '' : 'the owner could not add a member after a refusal');
  check('messages.refusal.persists', await refusal('not_a_member', () => revived.messages.admit(outsiderAuth, {
    idempotency_key: crypto.randomUUID(), retry_until: future(),
    message: { id: 'msg-out-01', sender: caeruliaNode, delivery_kind: '插话', msg: '我也能说话吗。', meta: { thread: thread.thread_ref } },
    target_endpoint: lazuliNode, plan,
  })));
  check('messages.refusal.changed_id', await refusal('id_conflict', () => revived.messages.admit(outsiderAuth, {
    idempotency_key: crypto.randomUUID(), retry_until: future(),
    message: { id: 'msg-out-01', sender: caeruliaNode, delivery_kind: '插话', msg: '换一句正文。', meta: { thread: thread.thread_ref } },
    target_endpoint: lazuliNode, plan,
  })));

  // A changed body under an accepted identity is refused without touching the original.
  const kept = await lazuli.node.messages.getByRef(lazuliAuth, { sender: dpdpNode, id: 'msg-01' });
  check('messages.receipt.unpoisoned', kept.message.msg === '请核对这次实验的结论。' ? '' : `the original body changed to ${kept.message.msg}`);

  const beforeSeq = (await lazuli.node.threads.describe(lazuliAuth, thread.thread_ref)).thread.next_seq;
  check('messages.mention.invalid', await refusal('invalid_request', () => lazuli.node.messages.admit(dpdpAuth, {
    idempotency_key: crypto.randomUUID(), retry_until: future(),
    message: { id: 'msg-03', sender: dpdpNode, delivery_kind: '找人', msg: '找错人了。', meta: { thread: thread.thread_ref, mentions: ['@nobody'] } },
    target_endpoint: lazuliNode, plan,
  })));
  const afterSeq = (await lazuli.node.threads.describe(lazuliAuth, thread.thread_ref)).thread.next_seq;
  check('messages.mention.no_seq', beforeSeq === afterSeq ? '' : `a refused admission consumed a seq: ${beforeSeq} -> ${afterSeq}`);
  check('messages.mention.no_entry', await refusal('not_found', () => lazuli.node.messages.getByRef(lazuliAuth, { sender: dpdpNode, id: 'msg-03' })));

  const attention = await lazuli.node.messages.attention(lazuliAuth, thread.thread_ref);
  check('messages.attention', attention.items.length === 1 && attention.items[0]!.recipient === lazuliNode ? '' : 'the owner did not commit the attention obligation');

  const inbox = await lazuli.node.messages.inbox(lazuliAuth);
  check('messages.inbox', inbox.items.length === 1 ? '' : `unexpected local inbox ${inbox.items.length}`);

  const obligation = attention.items[0]!.obligation_id;
  const acked = await lazuli.node.messages.ack(lazuliAuth, thread.thread_ref, obligation, mut());
  check('messages.ack', acked.acked ? '' : 'the obligation was not acknowledged');
  check('messages.ack.foreign', await refusal('not_found', () => lazuli.node.messages.ack(dpdpAuth, thread.thread_ref, obligation, mut())));

  const reply = await lazuli.node.messages.admit(lazuliAuth, {
    idempotency_key: crypto.randomUUID(), retry_until: future(),
    message: { id: 'msg-02', sender: lazuliNode, delivery_kind: '答复这条请求', msg: '第二组数据需要重算。', meta: { thread: thread.thread_ref, in_reply_to: { sender: send.message.sender, id: send.message.id }, relation: 'reply' } },
    target_endpoint: lazuliNode, plan,
  });
  check('messages.reply', reply.seq === '2' ? '' : `unexpected reply seq ${reply.seq}`);

  const permit = await lazuli.node.threads.permits(dpdpAuth, {
    message_ref: { sender: send.message.sender, id: send.message.id }, grantee: lazuliNode,
    landing: thread.thread_ref, expires_at: future(120_000), remaining: 1, revoked: false, followup: false,
  }, mut());
  check('thread.permit', permit.remaining === 1 ? '' : 'the reply permit was not stored');

  check('owner.guard', await refusal('forbidden', () => lazuli.node.threads.close(dpdpAuth, thread.thread_ref, mut())));

  // RFC 0003 §6: a reply that names no thread lands in the thread holding the original
  // message. Every reply used to open a new first-contact thread, so two nodes could not
  // read one exchange back from a single thread (dpdp report 4, problem 1). Last, because it
  // adds an obligation to the thread the earlier checks count.
  const implicit = await lazuli.node.messages.admit(lazuliAuth, {
    idempotency_key: crypto.randomUUID(), retry_until: future(),
    message: { id: 'msg-02b', sender: lazuliNode, delivery_kind: '补充说明', msg: '这条回复不指定 thread。', meta: { in_reply_to: { sender: send.message.sender, id: send.message.id }, relation: 'followup' } },
    target_endpoint: lazuliNode, plan,
  });
  check('messages.reply.landing', implicit.thread_ref.id === thread.thread_ref.id && implicit.seq === '3'
    ? '' : `the unnamed reply landed in ${implicit.thread_ref.owner}/${implicit.thread_ref.id} seq ${implicit.seq}`);
}

// =============================================================================
// 2. two real HTTP nodes: delivery, remote attention, await, node recreation
// =============================================================================
async function phaseNet(): Promise<void> {
  // Both nodes get their real base_url before construction; each resolves the other live.
  const netA = serve(lazuliNode, base_url => rig(lazuliNode, base_url, { mayAdmit: async () => true }));
  const netB = serve(dpdpNode, base_url => rig(dpdpNode, base_url, { mayAdmit: async () => true }));
  const nodeA = netA.node;
  const nodeB = netB.node;
  const authA: Auth = { actor: lazuliNode };
  const authB: Auth = { actor: dpdpNode };

  const thread = await nodeB.threads.create(authB, { ...mut(), title: 'federated review' });
  const ref = thread.thread_ref;
  check('net.thread.create', ref.owner === dpdpNode ? '' : `unexpected owner ${JSON.stringify(ref)}`);

  await nodeB.threads.member(authB, ref, lazuliNode, { access: ['post', 'read'] }, mut());
  const broadcast = await nodeB.messages.admit(authB, {
    idempotency_key: crypto.randomUUID(), retry_until: future(),
    message: { id: 'net-01', sender: dpdpNode, delivery_kind: '通知全体', msg: '全体注意第二组数据。', meta: { thread: ref, mentions: ['@all'] } },
    target_endpoint: dpdpNode, plan,
  });
  check('net.broadcast.admit', broadcast.seq === '1' ? '' : `unexpected seq ${broadcast.seq}`);

  // Until the owner's grant notice lands, the member knows no owner to reconcile against.
  await nodeA.client.reconcile();
  check('net.grant.owner_unknown', (await nodeA.messages.inbox(authA)).items.length === 0
    ? '' : 'the member reconciled a thread whose owner it was never granted');
  // The owner pushes its unacked notices over the member's /grants endpoint.
  await nodeB.client.maintain();
  await nodeA.client.reconcile();
  const local = await nodeA.messages.inbox(authA);
  const item = local.items.find(row => row.attention.message_ref.id === 'net-01');
  check('net.attention.persisted', item !== undefined && item.attention.seq === '1'
    && item.attention.entry?.message.msg === '全体注意第二组数据。' ? '' : 'the member did not persist the remote obligation');
  check('net.attention.injected', item?.injected === true ? '' : 'the runtime never consumed the remote entry');
  check('net.attention.acked', item?.attention.acked === true ? '' : 'the member did not ack the remote obligation');
  // `@all` makes the owner a recipient too. The owner's own client maintains that row, so the
  // proof it survived is the local inbox record, not an unacked owner-side obligation.
  const ownerInbox = await nodeB.messages.inbox(authB);
  check('net.attention.owner_sees_own',
    ownerInbox.items.some(row => row.attention.recipient === dpdpNode && row.attention.message_ref.id === 'net-01')
      ? '' : 'the owner lost its own obligation');
  const peerUnacked = await nodeB.messages.attention({ actor: lazuliNode }, ref);
  check('net.attention.cleared', peerUnacked.items.length === 0 ? '' : `the owner still holds ${peerUnacked.items.length} unacked obligations for the member`);

  const ask = {
    id: 'net-02', sender: lazuliNode, delivery_kind: '请求复核', msg: '请复核第二组数据。',
    meta: { thread: ref, mentions: ['@all'] },
  };
  // The awaited reply is consumed by the sender's own session, not by the
  // receiver-side execution context named in the wire plan.
  const pending = nodeA.client.send(dpdpNode, ask, awaitPlan, { context: 'Main' });
  const landed = await waitFor(async () =>
    (await nodeB.messages.list(authB, ref, { attention: 'thread' })).items.some(entry => entry.message.id === 'net-02'));
  check('net.send.admitted', landed ? '' : 'the peer never admitted the sent message');

  await nodeB.messages.admit(authB, {
    idempotency_key: crypto.randomUUID(), retry_until: future(),
    message: { id: 'net-03', sender: dpdpNode, delivery_kind: '答复这条请求', msg: '复核完成，数据可用。', meta: { thread: ref, in_reply_to: { sender: lazuliNode, id: 'net-02' }, relation: 'reply' } },
    target_endpoint: dpdpNode, plan,
  });
  const outcome = await polling(pending, () => nodeA.client.reconcile());
  check('net.send.await', outcome.admission === 'accepted' && outcome.wait === 'answered'
    && outcome.reply?.message.msg === '复核完成，数据可用。' && outcome.reply_body === undefined
    && !netA.shared.injected.some(entry => entry.message.id === 'net-03')
    ? '' : `await did not resolve: admission=${outcome.admission} wait=${outcome.wait ?? 'none'} reply=${outcome.reply?.message.msg ?? 'none'}`);
  check('net.send.receipt', outcome.receipt?.seq === '2' && outcome.receipt?.guarantee === 'binding-defined'
    ? '' : `unexpected receipt ${JSON.stringify(outcome.receipt)}`);

  // The host confirms the tool result it rendered; only that confirmation advances the shared
  // record, never the submission itself (RFC 0008 §5).
  if (outcome.reply !== undefined && outcome.reply_body === undefined) {
    await nodeA.client.confirmToolResult(outcome.reply, 'Main');
  }
  await nodeA.client.reconcile();

  // RFC 0008 §5: the awaited reply body and the automatic fragment share one consumption
  // record. The first await renders the body; the fragment that follows must not render it
  // a second time, so the entry is recorded consumed without a second injection.
  const awaitedReplySeq = outcome.reply?.seq ?? '';
  const fragmentRow = (await nodeA.messages.inbox(authA)).items
    .find(row => row.attention.seq === awaitedReplySeq && row.attention.entry !== undefined);
  const injectedOnce = netA.shared.injected
    .filter(entry => entry.seq === awaitedReplySeq && entry.thread_ref.id === ref.id).length;
  // Whichever path confirmed first owns the single rendering: the tool result when the body
  // came back inline, the fragment when it had already been consumed. The invariant is one.
  const inlineBody = outcome.reply_body === undefined ? 1 : 0;
  check('net.send.await.body_once', injectedOnce + inlineBody === 1
    ? '' : `the reply body was rendered ${injectedOnce + inlineBody} times (fragment=${injectedOnce}, tool=${inlineBody})`);
  check('net.send.await.no_second_body', fragmentRow?.injected === true && fragmentRow.context === 'Main'
    ? '' : 'the reply obligation never reached a delivered state');

  // The reverse order: the automatic fragment consumes the body first, so the awaited tool
  // result that follows reports the arrival facts instead of rendering the body a second time.
  const askAgain = {
    id: 'net-06', sender: lazuliNode, delivery_kind: '第二次请求', msg: '再确认一次。',
    meta: { thread: ref, mentions: ['@all'] },
  };
  // The sender names its own session as the consumer, so the fragment record and
  // the tool result record resolve to the same slot.
  // Send without waiting first, so the automatic fragment is the path that consumes
  // the reply before a later await registers for the same original.
  const firstSend = await nodeA.client.send(dpdpNode, askAgain, plan, { context: 'Main' });
  check('net.send.await.fragment_first_admitted', firstSend.admission === 'accepted'
    ? '' : `the fragment-first request was not admitted: ${firstSend.admission}`);
  await waitFor(async () =>
    (await nodeB.messages.list(authB, ref, { attention: 'thread' })).items.some(entry => entry.message.id === 'net-06'));
  await nodeB.messages.admit(authB, {
    idempotency_key: crypto.randomUUID(), retry_until: future(),
    message: { id: 'net-07', sender: dpdpNode, delivery_kind: '答复第二次', msg: '第二次确认完成。', meta: { thread: ref, in_reply_to: { sender: lazuliNode, id: 'net-06' }, relation: 'reply' } },
    target_endpoint: dpdpNode, plan,
  });
  // The fragment path consumes it first: the reply obligation is stored locally, then the
  // runtime consumes it as a fragment.
  await nodeA.client.reconcile();
  const storedReply = await waitFor(async () =>
    (await nodeA.messages.inbox(authA)).items.some(row => row.attention.entry?.message.id === 'net-07'));
  check('net.send.await.fragment_stored', storedReply ? '' : 'the reply obligation never landed locally');
  const fragmentConsumed = await waitFor(async () => {
    const row = (await nodeA.messages.inbox(authA)).items.find(item => item.attention.entry?.message.id === 'net-07');
    return row?.injected === true;
  });
  // A later await for the same original finds the reply already consumed by that fragment,
  // so it reports arrival facts without rendering the body a second time.
  const secondReply = await nodeA.client.send(dpdpNode, askAgain, awaitPlan, { context: 'Main' });
  const fragmentFirst = netA.shared.injected.some(entry => entry.message.id === 'net-07');
  check('net.send.await.withheld_after_fragment', secondReply.wait === 'answered'
    && secondReply.reply?.message.id === 'net-07' && secondReply.reply_body === 'withheld' && fragmentFirst && fragmentConsumed
    ? '' : `the tool result repeated a body the fragment had shown: body=${secondReply.reply_body ?? 'none'} fragmentFirst=${fragmentFirst} fragmentConsumed=${fragmentConsumed}`);

  const replayed = await nodeA.client.send(dpdpNode, ask, plan);
  check('net.send.replay', replayed.admission === 'accepted' && replayed.receipt?.receipt_id === outcome.receipt?.receipt_id
    && replayed.receipt?.seq === outcome.receipt?.seq
    ? '' : `a re-drive produced ${JSON.stringify(replayed.receipt)} instead of ${JSON.stringify(outcome.receipt)}`);
  const afterReplay = await nodeB.threads.describe(authB, ref);
  check('net.send.replay.no_append', afterReplay.thread.next_seq === '6' ? '' : `the re-drive appended an entry: next_seq ${afterReplay.thread.next_seq}`);

  // An obligation queued before the thread closes still reaches the member afterwards.
  netA.shared.allow = false;
  await nodeB.messages.admit(authB, {
    idempotency_key: crypto.randomUUID(), retry_until: future(),
    message: { id: 'net-04', sender: dpdpNode, delivery_kind: '收尾通知', msg: '收尾前最后一条。', meta: { thread: ref, mentions: ['@all'] } },
    target_endpoint: dpdpNode, plan,
  });
  await nodeB.threads.close(authB, ref, mut());
  await nodeA.client.reconcile();
  const queued = (await nodeA.messages.inbox(authA)).items.find(row => row.attention.message_ref.id === 'net-04');
  check('net.closed.queued', queued !== undefined && queued.attention.acked === true
    ? '' : 'an obligation queued before close did not reach the member');
  const closedOutcome = await nodeA.client.send(dpdpNode, {
    id: 'net-05', sender: lazuliNode, delivery_kind: '补充', msg: '还有一件事。', meta: { thread: ref },
  }, plan);
  check('net.closed.refusal', closedOutcome.admission === 'refused' && closedOutcome.error?.code === 'thread_closed'
    ? '' : `a closed thread answered ${closedOutcome.admission}/${closedOutcome.error?.code ?? 'none'}`);

  // The same Storage under a new node object: obligations survive, injection resumes.
  netA.shared.allow = true;
  const nodeA2 = netA.recreate();
  const authA2: Auth = { actor: lazuliNode };
  const carried = await nodeA2.messages.inbox(authA2);
  check('net.recreate.inbox', carried.items.length === 6 && carried.items.some(row => row.attention.message_ref.id === 'net-04')
    ? '' : `the recreated node saw ${carried.items.length} obligations: ${carried.items.map(row => row.attention.message_ref.id).join(',')}`);
  const injectedBefore = netA.shared.injected.length;
  await nodeA2.client.maintain();
  const requeued = (await nodeA2.messages.inbox(authA2)).items.find(row => row.attention.message_ref.id === 'net-04');
  check('net.recreate.reinject', requeued?.injected === true && netA.shared.injected.length > injectedBefore
    ? '' : 'maintain did not re-inject the entry the runtime never consumed');
  await nodeA2.client.reconcile();
  const rows = (await nodeA2.messages.inbox(authA2)).items;
  check('net.recreate.all_acked', rows.length === 6 && rows.every(row => row.attention.acked)
    ? '' : `${rows.filter(row => !row.attention.acked).length} of ${rows.length} obligations stayed unacked`);
}

// =============================================================================
// 3. outbound recovery: a pending delivery re-driven after the peer comes back
// =============================================================================
async function phaseRecoverOutbound(): Promise<void> {
  // Nothing listens here until the peer comes back; the sender keeps a durable record meanwhile.
  const dead = 'http://127.0.0.1:1/onlyne/v3';
  let target = dead;

  const receiver = rig(receiverNode, dead).node;
  const authR: Auth = { actor: receiverNode };
  const thread = await receiver.threads.create(authR, { ...mut(), title: 'late delivery' });
  await receiver.threads.member(authR, thread.thread_ref, senderNode, { access: ['post', 'read'] }, mut());

  const sender = rig(senderNode, 'http://127.0.0.1:1/onlyne/v3', {
    resolve: async (): Promise<Target> => ({ node_id: receiverNode, base_url: target, headers: { 'x-onlyne-actor': senderNode } }),
  });
  const message = {
    id: 'rec-01', sender: senderNode, delivery_kind: '补投', msg: '这条消息要等到对端起来。',
    meta: { thread: thread.thread_ref },
  };
  const first = await sender.node.client.send(receiverNode, message, plan);
  check('recover.outbound.unknown', first.admission === 'unknown' && first.error?.code === 'unavailable'
    && first.receipt === undefined ? '' : `an unreachable peer reported ${first.admission}/${first.error?.code ?? 'none'}`);
  check('recover.outbound.not_delivered',
    (await receiver.messages.list(authR, thread.thread_ref, { attention: 'thread' })).items.length === 0
    ? '' : 'the message reached the peer while its port was closed');

  let handler: (request: Request) => Promise<Response> = async () => new Response('starting', { status: 503 });
  const server = Bun.serve({ port: 0, hostname: '127.0.0.1', fetch: request => handler(request) });
  servers.push(server);
  handler = request => receiver.fetch(request);
  target = `http://127.0.0.1:${server.port}/onlyne/v3`;

  const restarted = sender.recreate();
  await restarted.client.recover();
  const delivered = await receiver.messages.list(authR, thread.thread_ref, { attention: 'thread' });
  check('recover.outbound.redriven', delivered.items.length === 1 && delivered.items[0]!.message.id === 'rec-01'
    ? '' : `recover did not re-drive the pending delivery: ${delivered.items.length} entries`);
  await restarted.client.recover();
  const twice = await receiver.messages.list(authR, thread.thread_ref, { attention: 'thread' });
  check('recover.outbound.idempotent', twice.items.length === 1 ? '' : `a second recover duplicated the delivery: ${twice.items.length} entries`);
  const receipt = await restarted.client.send(receiverNode, message, plan);
  check('recover.outbound.receipt', receipt.admission === 'accepted' && receipt.receipt?.seq === '1'
    ? '' : `the recovered outbound reported ${receipt.admission}/${JSON.stringify(receipt.receipt)}`);
}

// =============================================================================
// 4. expiry: obligations outlive their bodies and still accept an ack
// =============================================================================
async function phaseExpiry(): Promise<void> {
  const window = future(30_000);
  const thread = await expiring.node.threads.create(lazuliAuth, { idempotency_key: crypto.randomUUID(), retry_until: window, title: 'expiring' });
  const expiringMessage = {
    id: 'msg-03', sender: lazuliNode, delivery_kind: '过期前', msg: '过期后只剩墓碑。',
    meta: { thread: thread.thread_ref, mentions: ['@lazuli'] },
  };
  const expired = await expiring.node.messages.admit(lazuliAuth, {
    idempotency_key: crypto.randomUUID(), retry_until: window,
    message: expiringMessage, target_endpoint: lazuliNode, plan,
  });
  clock.value += 60_000;
  const expiry = await expiring.node.messages.expire(lazuliAuth, thread.thread_ref, '2');
  check('messages.expire', expiry.expired_before === '2' ? '' : 'expiry did not run');

  const afterExpiry = await expiring.node.messages.attention(lazuliAuth, thread.thread_ref, { page_size: 100 });
  const row = afterExpiry.items.find(item => item.seq === expired.seq);
  check('messages.expire.disposition', row?.disposition === 'expired' ? '' : 'the expired obligation lost its explicit outcome');
  check('messages.expire.replay', await refusal('retry_window_expired', () => expiring.node.messages.admit(lazuliAuth, {
    idempotency_key: crypto.randomUUID(), retry_until: future(),
    message: expiringMessage, target_endpoint: lazuliNode, plan,
  })));
  check('messages.expire.ack', row === undefined
    ? 'the expired obligation is no longer listed'
    : await succeeds(() => expiring.node.messages.ack(lazuliAuth, thread.thread_ref, row.obligation_id, mut())));
}

// =============================================================================
// 5. resources: handover, revision fence, old events, backlog expiry, replay
// =============================================================================
async function phaseResources(): Promise<void> {
  const executions = new Map<string, number>();
  const driver: ResourceDriver = {
    validate: async (resource, operation) => {
      if (!resource.operations.includes(operation)) throw new OnlyneError('unsupported_delivery', `${operation} is not declared.`);
    },
    execute: async (resource, request) => {
      executions.set(request.operation_id, (executions.get(request.operation_id) ?? 0) + 1);
      return { state: 'committed', data: { operation: request.operation, revision: resource.revision } };
    },
  };
  const keeper = serve(keeperNode, base_url => rig(keeperNode, base_url, { resources: driver, mayHandover: async () => true }));
  const node = keeper.node;
  const authK: Auth = { actor: keeperNode };
  const authD: Auth = { actor: dpdpNode };

  const resource: Resource = {
    resource_ref: { owner: keeperNode, id: 'chat-01' }, kind: 'im_conversation', operations: ['read', 'send', 'handover'],
    assignee: keeperNode, readers: [keeperNode, dpdpNode], controllers: [keeperNode],
    event_recipients: [keeperNode], result_recipients: [keeperNode],
    event_seq: '0', revision: 'rev-a', retention_ms: 60_000,
  };
  const ref = resource.resource_ref;
  const registered = await node.resources.register(authK, resource);
  check('resources.register', registered.revision === 'rev-a' ? '' : 'the resource did not register');

  const event = await node.resources.receive(authK, ref, { source_id: 'platform-1', data: { text: '图里的单位是什么' } });
  const firstEventId = event.event_id;
  check('resources.receive', firstEventId === '1' && event.recipients[0] === keeperNode
    ? '' : `unexpected event id ${firstEventId} for a fresh resource`);

  const handover = await node.resources.operate(authK, ref, {
    ...mut(), operation_id: crypto.randomUUID(),
    operation: 'handover', expected_revision: 'rev-a',
    data: { target: dpdpNode, decision_deadline: future() },
  });
  const operationId = handover.operation_id;
  check('resources.handover.initiate', handover.state === 'pending'
    ? '' : `initiation reported ${handover.state} instead of pending`);

  // Backlog blocks the commit: the target accepts while an event is undelivered.
  let blocked: string | null = null;
  try {
    await node.resources.decideHandover(authD, ref, operationId, true, mut());
  } catch (error) {
    blocked = codeOf(error);
  }
  const held = await node.resources.describe(authK, ref);
  check('resources.handover.backlog', held.assignee === keeperNode && held.revision === 'rev-a'
    ? (blocked === null || blocked === 'conflict' ? '' : `an accept behind backlog failed with ${blocked} instead of conflict`)
    : `backlog did not block the commit: ${JSON.stringify(held)}`);

  const acknowledged = await node.resources.ack(authK, ref, firstEventId, mut());
  check('resources.ack', acknowledged.acknowledged.includes(keeperNode) ? '' : 'the event stayed in the backlog');

  const committed = await node.resources.decideHandover(authD, ref, operationId, true, mut());
  check('resources.handover.commit', committed.state === 'committed' && committed.cutoff_event_id === firstEventId
    ? '' : `unexpected commit ${JSON.stringify(committed)}`);

  const moved = await node.resources.describe(authK, ref);
  check('resources.handover.assignee', moved.assignee === dpdpNode ? '' : 'the assignee did not move');
  check('resources.handover.revision', moved.revision !== 'rev-a' ? '' : 'the commit did not move the revision');

  const old = await node.resources.event(authK, ref, firstEventId);
  check('resources.event.old_read', old.event_id === firstEventId && old.revision === 'rev-a'
    && old.recipients.includes(keeperNode) ? '' : 'the former assignee lost its first-attribution read');

  check('resources.fence.revision', await refusal('revision_conflict', () => node.resources.operate(authD, ref, {
    ...mut(), operation_id: crypto.randomUUID(),
    operation: 'send', expected_revision: 'rev-a', data: { text: '旧版本发送' },
  })));

  // A backlog event past its retention gets an explicit expiry and stops blocking handover.
  const late = await node.resources.receive(authK, ref, {
    source_id: 'platform-2', data: { text: '第二条' }, expires_at: new Date(clock.value - 1_000).toISOString(),
  });
  await node.resources.recover();
  const swept = await node.resources.event(authK, ref, late.event_id);
  check('resources.recover.expiry', swept.expired === true
    ? '' : `the retention sweep left event ${late.event_id} without a terminal disposition`);

  const back = await node.resources.operate(authD, ref, {
    ...mut(), operation_id: crypto.randomUUID(),
    operation: 'handover', expected_revision: moved.revision,
    data: { target: keeperNode, decision_deadline: future() },
  });
  const returned = await node.resources.decideHandover(authK, ref, back.operation_id, true, mut());
  check('resources.handover.after_expiry', returned.state === 'committed'
    && (await node.resources.describe(authK, ref)).assignee === keeperNode
    ? '' : `handover after an expiry sweep did not commit: ${JSON.stringify(returned)}`);

  // RFC 0006 §6 / 0012 §5: a first-recipient ack arriving after retention expiry keeps its
  // receipt evidence, clears any leftover once, and never revives the expired obligation.
  const afterExpiry = await node.resources.receive(authK, ref, {
    source_id: 'platform-3', data: { text: '过期之后到达的确认' }, expires_at: new Date(clock.value - 1_000).toISOString(),
  });
  await node.resources.recover();
  const terminal = await node.resources.event(authK, ref, afterExpiry.event_id);
  check('resources.expire.terminal', terminal.expired === true ? '' : 'retention left the obligation without a terminal disposition');
  const lateAck = await node.resources.ack(authK, ref, afterExpiry.event_id, mut());
  check('resources.expire.late_ack_evidence', lateAck.expired === true && lateAck.acknowledged.includes(keeperNode)
    ? '' : `a late ack lost its receipt evidence: ${JSON.stringify(lateAck)}`);
  const repeat = await node.resources.ack(authK, ref, afterExpiry.event_id, mut());
  check('resources.expire.late_ack_idempotent',
    repeat.acknowledged.filter(actor => actor === keeperNode).length === 1
      ? '' : `a repeated ack recorded the receipt twice: ${JSON.stringify(repeat.acknowledged)}`);

  // One operation id executes once; the replay reads back the recorded result.
  const request = { ...mut(), operation_id: crypto.randomUUID(), operation: 'read' };
  const first = await node.resources.operate(authK, ref, request);
  const second = await node.resources.operate(authK, ref, request);
  const runs = executions.get(request.operation_id) ?? 0;
  check('resources.operation.replay', first.state === 'committed' && second.state === 'committed'
    && JSON.stringify(second.data) === JSON.stringify(first.data) && runs === 1
    ? '' : `replay executed ${runs} times and reported ${first.state}/${second.state}`);
  const record = await node.resources.operation(authK, ref, request.operation_id);
  check('resources.operation.record', record.result.state === 'committed' && record.actor === keeperNode
    ? '' : `the operation record reported ${record.result.state}`);

  // Two callers race on one operation id: at most one external effect, no third state.
  const raced = { ...mut(), operation_id: crypto.randomUUID(), operation: 'read' };
  const [left, right] = await Promise.all([
    node.resources.operate(authK, ref, raced),
    node.resources.operate(authK, ref, raced),
  ]);
  const races = executions.get(raced.operation_id) ?? 0;
  const settled = [left.state, right.state];
  check('resources.operation.concurrent', races === 1
    && settled.every(state => state === 'committed' || state === 'unknown')
    && settled.includes('committed')
    ? '' : `one operation id executed ${races} times and settled as ${settled.join('/')}`);
}


// =============================================================================
// 6. RFC 0017: node authentication, exchange transcripts and request signatures
// =============================================================================
async function phaseAuth(): Promise<void> {
  const keyed = (node_id: string, base_url: string, seeds: TrustSeed[] = [], extra: Partial<Plugins> = {}, now: () => number = () => clock.value, replaceGate?: Promise<void>, replaceFails?: 'after-swap'): Rig => {
    const storage = new MemoryStorage();
    const shared: Shared = { injected: [], allow: true };
    let material = generateKeyMaterial();
    const keysHook = Object.assign(async () => material, {
      replace: async (next: KeyMaterial) => {
        if (replaceGate !== undefined) await replaceGate;
        material = next;
        if (replaceFails === 'after-swap') throw new Error('the key hook failed after writing');
      },
    });
    const plugins: Plugins = {
      storage,
      resolve: async (node: string): Promise<Target> => ({ node_id: node, base_url: urls[node] ?? base_url }),
      authenticate: async (): Promise<Auth | null> => null,
      decide: async () => { throw new OnlyneError('decision_unavailable', 'No plugin decision.'); },
      keys: keysHook,
      trust_seeds: seeds,
      mayAdmit: async () => true,
      inboundThread: async () => true,
      runtime: {
        supports: async () => true,
        notify: event => notified.push({ node: node_id, event }),
        inject: async (entry: Entry, context?: string) => {
          shared.injected.push(entry);
          return { context: context ?? node_id, consumed: shared.allow };
        },
        todo: async () => {},
      },
      ...extra,
    };
    const options: NodeOptions = { node_id, base_url, plugins, now, id: () => crypto.randomUUID(), wait_ms: 5_000 };
    return { node: new OnlyneNode(options), storage, shared, recreate: () => new OnlyneNode({ ...options }) };
  };

  const authA = serve(lazuliNode, base_url => keyed(lazuliNode, base_url));
  const authB = serve(dpdpNode, base_url => keyed(dpdpNode, base_url));
  const nodeA = authA.node;
  const nodeB = authB.node;
  const authAOwner: Auth = { actor: lazuliNode };
  const authBOwner: Auth = { actor: dpdpNode };

  const identityA = (await nodeA.keys.describe())!;
  const identityB = (await nodeB.keys.describe())!;
  check('auth.keys.describe', identityA.fingerprint.length === 16 && identityA.node_id === lazuliNode
    ? '' : `unexpected own key description ${JSON.stringify(identityA)}`);

  // The public identity route is what a peer reads before exchanging.
  const published = await (await fetch(`${urls[lazuliNode]}/keys`)).json() as { public_key: string; fingerprint: string; node_id: string };
  check('auth.keys.published', published.fingerprint === identityA.fingerprint && published.node_id === lazuliNode
    ? '' : `published ${JSON.stringify(published)}`);

  // A's request pins B's fingerprint; B answers with its own approval statement.
  const entryA = await nodeA.trust.request(authAOwner, dpdpNode, identityB.fingerprint);
  // A approved locally; B has not approved A yet, so the row is pending on that half.
  check('auth.exchange.pending_both', entryA.state === 'pending' && entryA.local_approved && !entryA.remote_approved
    ? '' : `A recorded ${JSON.stringify(entryA)}`);
  const entryB = await nodeB.trust.entry(lazuliNode);
  check('auth.exchange.pending_peer', entryB?.state === 'pending' && entryB.local_approved === false && entryB.remote_approved === true
    ? '' : `B recorded ${JSON.stringify(entryB)}`);
  // The phase is the host-facing projection; a host never re-derives it from the halves.
  check('auth.phase.requested', trustPhase(entryA) === 'requested' ? '' : `A phase ${trustPhase(entryA)}`);
  check('auth.phase.invited', entryB !== undefined && trustPhase(entryB) === 'invited' ? '' : `B phase ${trustPhase(entryB ?? entryA)}`);

  // A tampered transcript is refused: the signature covers sender, recipient and key.
  check('auth.exchange.forged', await refusal('unauthenticated', async () => {
    const statement = { node_id: caeruliaNode, target: dpdpNode, public_key: identityA.public_key,
      target_fingerprint: identityB.fingerprint, base_url: urls[lazuliNode]!, approved: true,
      nonce: crypto.randomUUID(), timestamp: clock.value, signature: 'AAAA' };
    const answer = await fetch(`${urls[dpdpNode]}/introductions`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(statement) });
    const body = await answer.json() as { error?: { code: ErrorCode; message: string } };
    throw new OnlyneError(body.error?.code ?? 'internal', body.error?.message ?? 'accepted');
  }));

  // A captured transcript cannot be replayed: the nonce is spent.
  check('auth.exchange.replay', await refusal('unauthenticated', async () => {
    const captured = await nodeB.trust.entry(lazuliNode);
    const replay = { node_id: lazuliNode, target: dpdpNode, public_key: identityA.public_key,
      target_fingerprint: identityB.fingerprint, base_url: urls[lazuliNode]!, approved: true,
      nonce: crypto.randomUUID(), timestamp: clock.value, signature: 'AAAA' };
    void captured;
    const answer = await fetch(`${urls[dpdpNode]}/introductions`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(replay) });
    const body = await answer.json() as { error?: { code: ErrorCode; message: string } };
    if (body.error === undefined) throw new OnlyneError('internal', 'an unsigned transcript was accepted');
    throw new OnlyneError(body.error.code, body.error.message);
  }));

  // Both sides confirm; a signed cross-node send then passes, and B's answer is signed too.
  await nodeA.trust.accept(authAOwner, dpdpNode, identityB.fingerprint);
  await nodeB.trust.accept(authBOwner, lazuliNode, identityA.fingerprint);
  check('auth.exchange.trusted', (await nodeA.trust.entry(dpdpNode))?.state === 'trusted'
    && (await nodeB.trust.entry(lazuliNode))?.state === 'trusted' ? '' : 'the exchange did not reach trusted on both sides');
  check('auth.phase.exchanged', trustPhase((await nodeA.trust.entry(dpdpNode))!) === 'exchanged'
    && trustPhase((await nodeB.trust.entry(lazuliNode))!) === 'exchanged' ? '' : 'a trusted pair did not project as exchanged');

  // Every committed transition reached the host's one notification channel.
  const eventsFor = (node: string) => notified
    .filter(row => row.node === node)
    .map(row => row.event as { event: string; cause?: string; phase?: string });
  check('auth.notify.requested', eventsFor(lazuliNode).some(e => e.event === 'trust' && e.cause === 'local-request' && e.phase === 'requested')
    ? '' : `A notified ${JSON.stringify(eventsFor(lazuliNode))}`);
  check('auth.notify.invited', eventsFor(dpdpNode).some(e => e.event === 'trust' && e.cause === 'peer-statement' && e.phase === 'invited')
    ? '' : `B notified ${JSON.stringify(eventsFor(dpdpNode))}`);

  const signed = await nodeA.client.send(dpdpNode, {
    id: 'auth-01', sender: lazuliNode, delivery_kind: '签名验证', msg: '这条消息经过签名投递。',
  }, plan);
  check('auth.signed_send', signed.admission === 'accepted' ? '' : `a signed send was ${signed.admission}: ${JSON.stringify((signed as { error?: unknown }).error ?? null)}`);
  check('auth.signed_landed', (await nodeB.messages.inbox(authBOwner)).items.some(row => row.attention.message_ref.id === 'auth-01')
    ? '' : 'the signed message never landed on B');

  // Rotation: the new key is proven by the old one, and later sends use it.
  const rotated = await nodeA.trust.rotate(authAOwner);
  check('auth.rotate', rotated.fingerprint !== identityA.fingerprint ? '' : 'rotation kept the old key');
  const rotatedEntry = await nodeB.trust.entry(lazuliNode);
  check('auth.rotate.peer', rotatedEntry?.fingerprint === rotated.fingerprint && rotatedEntry.history?.includes(identityA.fingerprint)
    ? '' : `B recorded ${JSON.stringify(rotatedEntry)}`);
  // A proven rotation keeps both sides exchanged: the identity is continuous, so B does not
  // drop its own approval and A's statement carries A's approval (RFC 0017 §3.3).
  check('auth.rotate.phase', rotatedEntry !== undefined && trustPhase(rotatedEntry) === 'exchanged'
    && trustPhase((await nodeA.trust.entry(dpdpNode))!) === 'exchanged'
    ? '' : `after rotation B=${rotatedEntry === undefined ? 'none' : trustPhase(rotatedEntry)}`);
  await nodeB.trust.revoke(authBOwner, lazuliNode);
  const afterRevoke = await nodeA.client.send(dpdpNode, {
    id: 'auth-02', sender: lazuliNode, delivery_kind: '撤销后', msg: '撤销后不应送达。',
  }, plan);
  check('auth.revoked', afterRevoke.admission !== 'accepted' ? '' : 'a revoked peer was still admitted');

  // Revocation is one-sided until the revoked node learns of it: B keeps its row revoked, and
  // A's re-confirmation of an established relation is a local refresh that cannot resurrect it.
  await nodeA.trust.request(authAOwner, dpdpNode, identityB.fingerprint).catch(() => undefined);
  check('auth.revoked.sticky', (await nodeB.trust.entry(lazuliNode))?.state === 'revoked'
    ? '' : `B recorded ${JSON.stringify(await nodeB.trust.entry(lazuliNode))} after A re-confirmed`);
  check('auth.notify.revoked', eventsFor(dpdpNode).some(e => e.event === 'trust' && e.cause === 'local-revoke' && e.phase === 'revoked')
    ? '' : `B never announced the revocation: ${JSON.stringify(eventsFor(dpdpNode))}`);

  // One exchange is one object: the fingerprint shown is the one confirmation must repeat.
  const beginC = serve(keeperNode, base_url => keyed(keeperNode, base_url));
  const beginD = serve(senderNode, base_url => keyed(senderNode, base_url));
  const identityC = (await beginC.node.keys.describe())!;
  const identityD = (await beginD.node.keys.describe())!;
  const session = await beginC.node.trust.begin({ actor: keeperNode }, senderNode);
  check('auth.begin.identity', session.node_id === senderNode && session.fingerprint === identityD.fingerprint && session.public_key === identityD.public_key
    ? '' : `begin showed ${JSON.stringify(session)}`);
  check('auth.begin.fingerprint-required', await refusal('unauthenticated', () => session.confirm(identityC.fingerprint)));
  const confirmedC = await session.confirm(session.fingerprint);
  check('auth.begin.confirmed', trustPhase(confirmedC) === 'requested' ? '' : `C phase ${trustPhase(confirmedC)}`);
  const invitedD = await beginD.node.trust.entry(keeperNode);
  check('auth.begin.invited', invitedD !== undefined && trustPhase(invitedD) === 'invited' ? '' : `D phase ${trustPhase(invitedD ?? confirmedC)}`);

  // A failed introduction is owed work: the loop re-drives it once the retry window opens.
  let resolveCalls = 0;
  const redriveE = serve(caeruliaNode, base_url => keyed(caeruliaNode, base_url, [], {
    resolve: async (node: string): Promise<Target> => {
      if (node === receiverNode && ++resolveCalls === 2) throw new OnlyneError('unavailable', 'the introduction never left this node');
      return { node_id: node, base_url: urls[node] ?? base_url };
    },
  }));
  const redriveF = serve(receiverNode, base_url => keyed(receiverNode, base_url));
  const identityF = (await redriveF.node.keys.describe())!;
  const failedSend = await redriveE.node.trust.request({ actor: caeruliaNode }, receiverNode, identityF.fingerprint)
    .catch(error => error as OnlyneError);
  check('auth.redrive.marker', failedSend instanceof OnlyneError && failedSend.error.code === 'unavailable'
    ? '' : `the failing send reported ${String(failedSend)}`);
  check('auth.redrive.pending', await redriveE.node.client.hasPendingWork()
    ? '' : 'an unanswered introduction is not counted as pending work');
  check('auth.redrive.not-invited', (await redriveF.node.trust.entry(caeruliaNode)) === undefined
    ? '' : 'the peer recorded an introduction that never reached it');
  clock.value += EXCHANGE_RETRY_MS + 1;
  await redriveE.node.trust.redrive();
  const invitedF = await redriveF.node.trust.entry(caeruliaNode);
  check('auth.redrive.delivered', invitedF !== undefined && trustPhase(invitedF) === 'invited'
    ? '' : `F phase ${invitedF === undefined ? 'none' : trustPhase(invitedF)}`);
  check('auth.redrive.settled', !(await redriveE.node.client.hasPendingWork())
    ? '' : 'the attempt marker survived a delivered re-drive');

  // A peer that revoked this node refuses the introduction permanently. The retry marker
  // must not survive that refusal, or the loop would retry forever and never idle.
  await redriveF.node.trust.revoke({ actor: receiverNode }, caeruliaNode);
  const refusedAgain = await redriveE.node.trust.request({ actor: caeruliaNode }, receiverNode, identityF.fingerprint)
    .catch(error => error as OnlyneError);
  check('auth.revoked.refused', refusedAgain instanceof OnlyneError ? '' : 'a revoked peer accepted a new introduction');
  check('auth.revoked.no-retry', !(await redriveE.node.client.hasPendingWork())
    ? '' : 'a permanently refused introduction left the maintenance loop owing work');

  // Both sides approve, then the accepting side's statement is lost: the row it committed is
  // already `exchanged`, and only the maintenance loop can close the asymmetry (RFC 0017 §3.5,
  // §7 acceptance). No second confirmation may be required.
  let sendGate = true;
  let owedResolves = 0;
  const owed = serve(larchNode, base_url => keyed(larchNode, base_url));
  const owedPeer = serve(mossNode, base_url => keyed(mossNode, base_url, [], {
    resolve: async (node: string): Promise<Target> => {
      if (node === larchNode && ++owedResolves === 2 && sendGate) throw new OnlyneError('unavailable', 'the acceptance never left this node');
      return { node_id: node, base_url: urls[node] ?? base_url };
    },
  }));
  const identityMoss = (await owedPeer.node.keys.describe())!;
  const identityLarch = (await owed.node.keys.describe())!;
  await owed.node.trust.request({ actor: larchNode }, mossNode, identityMoss.fingerprint);
  check('auth.owed.invited', trustPhase((await owedPeer.node.trust.entry(larchNode))!) === 'invited'
    ? '' : `the peer read ${trustPhase((await owedPeer.node.trust.entry(larchNode))!)}`);
  const lostAccept = await owedPeer.node.trust.accept({ actor: mossNode }, larchNode, identityLarch.fingerprint)
    .catch(error => error as OnlyneError);
  check('auth.owed.dropped', lostAccept instanceof OnlyneError && lostAccept.error.code === 'unavailable'
    ? '' : `the dropped acceptance reported ${String(lostAccept)}`);
  check('auth.owed.asymmetric', trustPhase((await owedPeer.node.trust.entry(larchNode))!) === 'exchanged'
    && trustPhase((await owed.node.trust.entry(mossNode))!) === 'requested'
    ? '' : `states ${trustPhase((await owedPeer.node.trust.entry(larchNode))!)}/${trustPhase((await owed.node.trust.entry(mossNode))!)}`);
  check('auth.owed.pending', await owedPeer.node.client.hasPendingWork()
    ? '' : 'an approval the peer never received is not counted as owed work');
  // Network recovery: a fresh nonce, the peer records the statement and answers it, and the
  // obligation clears without another human confirmation.
  sendGate = false;
  clock.value += EXCHANGE_RETRY_MS + 1;
  await owedPeer.node.trust.redrive();
  check('auth.owed.converged', trustPhase((await owed.node.trust.entry(mossNode))!) === 'exchanged'
    ? '' : `the first node stayed ${trustPhase((await owed.node.trust.entry(mossNode))!)}`);
  check('auth.owed.settled', !(await owedPeer.node.client.hasPendingWork())
    ? '' : 'a settled delivery obligation still counts as owed work');

  // A first-contact statement that claims no approval is refused, not stored: with no
  // relationship there is nothing it could answer (RFC 0017 §3.3).
  const nettle = generateKeyMaterial();
  const nettlePublic = Buffer.from(nettle.public_key).toString('base64');
  const nettleBody = {
    node_id: nettleNode, target: mossNode, public_key: nettlePublic,
    target_fingerprint: (await owedPeer.node.keys.describe())!.fingerprint,
    base_url: 'http://127.0.0.1:9/onlyne/v3', approved: false,
    nonce: crypto.randomUUID(), timestamp: clock.value,
  };
  const nettleSignature = sign(null, Buffer.from(`onlyne-trust-v1\n${canonical(nettleBody)}`),
    createPrivateKey({ key: Buffer.from(nettle.private_key), format: 'der', type: 'pkcs8' })).toString('base64');
  check('auth.first-contact.declined', await refusal('invalid_request', async () => {
    const answer = await fetch(`${urls[mossNode]}/introductions`, {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ ...nettleBody, signature: nettleSignature }),
    });
    const body = await answer.json() as { error?: { code: ErrorCode; message: string } };
    throw new OnlyneError(body.error?.code ?? 'internal', body.error?.message ?? 'accepted');
  }));
  check('auth.first-contact.untouched', (await owedPeer.node.trust.entry(nettleNode)) === undefined
    ? '' : 'a declined first statement still created a trust row');

  // A failed verification is not a refusal: a peer whose clock is out of the window rejects
  // the statement, and the obligation stays owed for the next window (RFC 0017 §3.5).
  const skewed = serve(thistleNode, base_url => keyed(thistleNode, base_url, [], {}, () => clock.value + CLOCK_SKEW_MS * 2));
  const skewedFailure = await owed.node.trust.request({ actor: larchNode }, thistleNode, (await skewed.node.keys.describe())!.fingerprint)
    .catch(error => error as OnlyneError);
  check('auth.owed.verification-failure', skewedFailure instanceof OnlyneError && skewedFailure.error.code === 'unauthenticated'
    ? '' : `the skewed peer answered ${String(skewedFailure)}`);
  check('auth.owed.verification-retained', await owed.node.client.hasPendingWork()
    ? '' : 'a failed verification dropped the delivery obligation');

  // A repeat confirmation of an approved row is a refresh: timestamps move, nothing is sent,
  // and no obligation reappears (RFC 0017 §3.3).
  const beforeRefresh = owedResolves;
  const refreshed = await owedPeer.node.trust.accept({ actor: mossNode }, larchNode, identityLarch.fingerprint);
  check('auth.refresh.no-send', owedResolves === beforeRefresh + 1 && trustPhase(refreshed) === 'exchanged'
    ? '' : `the refresh resolved ${owedResolves - beforeRefresh} times and read ${trustPhase(refreshed)}`);
  check('auth.refresh.no-obligation', !(await owedPeer.node.client.hasPendingWork())
    ? '' : 'a refresh re-created a delivery obligation');

  // One rotation in flight: a second swap would replace the proof the peer still needs, so the
  // second call is refused until the first is acknowledged (RFC 0017 §2).
  let rotationGate = false;
  const rotating = serve('urn:onlyne:agent:rotating', base_url => keyed('urn:onlyne:agent:rotating', base_url, [], {
    resolve: async (node: string): Promise<Target> => {
      if (rotationGate && node === 'urn:onlyne:agent:rotated') throw new OnlyneError('unavailable', 'the rotation statement never left this node');
      return { node_id: node, base_url: urls[node] ?? base_url };
    },
  }));
  const rotatedPeer = serve('urn:onlyne:agent:rotated', base_url => keyed('urn:onlyne:agent:rotated', base_url));
  await rotating.node.trust.request({ actor: 'urn:onlyne:agent:rotating' }, 'urn:onlyne:agent:rotated', (await rotatedPeer.node.keys.describe())!.fingerprint);
  await rotatedPeer.node.trust.request({ actor: 'urn:onlyne:agent:rotated' }, 'urn:onlyne:agent:rotating', (await rotating.node.keys.describe())!.fingerprint);
  rotationGate = true;
  const beforeRotation = (await rotating.node.keys.describe())!.fingerprint;
  const unacknowledged = await rotating.node.trust.rotate({ actor: 'urn:onlyne:agent:rotating' });
  check('auth.rotate.unacknowledged', unacknowledged.fingerprint !== beforeRotation && await rotating.node.client.hasPendingWork()
    ? '' : 'an unacknowledged rotation left no delivery obligation');
  check('auth.rotate.second-refused', await refusal('conflict', () => rotating.node.trust.rotate({ actor: 'urn:onlyne:agent:rotating' })));
  rotationGate = false;
  clock.value += EXCHANGE_RETRY_MS + 1;
  await rotating.node.trust.redrive();
  check('auth.rotate.resumed', !(await rotating.node.client.hasPendingWork())
    ? '' : 'the acknowledged rotation left an obligation behind');

  // A candidate's service description is public: discovery works with keys enabled and no
  // trust row, so a candidate is never reported unreachable just because no exchange happened.
  const region = 'urn:onlyne:region:review';
  const discovering = serve('urn:onlyne:agent:discovering', base_url => keyed('urn:onlyne:agent:discovering', base_url, [], {
    domainCandidates: async () => ['urn:onlyne:agent:candidate'],
  }));
  const candidate = serve('urn:onlyne:agent:candidate', base_url => keyed('urn:onlyne:agent:candidate', base_url));
  await discovering.node.domains.join({ actor: 'urn:onlyne:agent:discovering' }, region, mut());
  await candidate.node.domains.join({ actor: 'urn:onlyne:agent:candidate' }, region, mut());
  const found = await discovering.node.domains.discover({ actor: 'urn:onlyne:agent:discovering' }, region);
  check('auth.discover.untrusted', found.members.some(row => row.node_id === 'urn:onlyne:agent:candidate') && found.unavailable.length === 0
    ? '' : `discovery returned ${JSON.stringify(found)}`);

  // Both sides rotate while the network is down. Each statement then addresses the peer's new
  // key and carries the old-key proof, so recovery needs no re-confirmation and no key reset.
  const realFetch = globalThis.fetch;
  let dropIntroductions = false;
  globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit) => {
    if (dropIntroductions && String(input).includes('/introductions')) throw new OnlyneError('unavailable', 'the introduction never left this node');
    return realFetch(input, init);
  }) as typeof fetch;
  try {
    const twinA = serve('urn:onlyne:agent:twin-a', base_url => keyed('urn:onlyne:agent:twin-a', base_url));
    const twinB = serve('urn:onlyne:agent:twin-b', base_url => keyed('urn:onlyne:agent:twin-b', base_url));
    await twinA.node.trust.request({ actor: 'urn:onlyne:agent:twin-a' }, 'urn:onlyne:agent:twin-b', (await twinB.node.keys.describe())!.fingerprint);
    await twinB.node.trust.request({ actor: 'urn:onlyne:agent:twin-b' }, 'urn:onlyne:agent:twin-a', (await twinA.node.keys.describe())!.fingerprint);
    dropIntroductions = true;
    const rotatedA = await twinA.node.trust.rotate({ actor: 'urn:onlyne:agent:twin-a' });
    const rotatedB = await twinB.node.trust.rotate({ actor: 'urn:onlyne:agent:twin-b' });
    check('auth.twin.pending', await twinA.node.client.hasPendingWork() && await twinB.node.client.hasPendingWork()
      ? '' : 'a rotation whose statement never left is not owed work');
    dropIntroductions = false;
    clock.value += EXCHANGE_RETRY_MS + 1;
    await twinA.node.trust.redrive();
    await twinB.node.trust.redrive();
    const seenByA = await twinA.node.trust.entry('urn:onlyne:agent:twin-b');
    const seenByB = await twinB.node.trust.entry('urn:onlyne:agent:twin-a');
    check('auth.twin.converged', seenByA?.fingerprint === rotatedB.fingerprint && seenByB?.fingerprint === rotatedA.fingerprint
      ? '' : `A saw ${seenByA?.fingerprint} B saw ${seenByB?.fingerprint}`);
    check('auth.twin.exchanged', seenByA !== undefined && trustPhase(seenByA) === 'exchanged'
      && seenByB !== undefined && trustPhase(seenByB) === 'exchanged'
      ? '' : `phases ${seenByA === undefined ? 'none' : trustPhase(seenByA)}/${seenByB === undefined ? 'none' : trustPhase(seenByB)}`);
    check('auth.twin.settled', !(await twinA.node.client.hasPendingWork()) && !(await twinB.node.client.hasPendingWork())
      ? '' : 'a converged rotation still counts as owed work');
  } finally {
    globalThis.fetch = realFetch;
  }

  // A refusal this node cannot authenticate must not destroy the approval: the statement is
  // parked, and the peer's next statement puts it back to work.
  const forgedPeer = serve('urn:onlyne:agent:forged-peer', base_url => keyed('urn:onlyne:agent:forged-peer', base_url));
  const victim = serve('urn:onlyne:agent:victim', base_url => keyed('urn:onlyne:agent:victim', base_url));
  globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit) => {
    if (String(input).includes('/introductions')) {
      return new Response(JSON.stringify({ error: { code: 'forbidden', message: 'not from the peer' } }), { status: 403, headers: { 'content-type': 'application/json' } });
    }
    return realFetch(input, init);
  }) as typeof fetch;
  try {
    const forged = await victim.node.trust.request({ actor: 'urn:onlyne:agent:victim' }, 'urn:onlyne:agent:forged-peer', (await forgedPeer.node.keys.describe())!.fingerprint)
      .catch(error => error as OnlyneError);
    check('auth.forged-refusal.reported', forged instanceof OnlyneError && forged.error.code === 'forbidden'
      ? '' : `the forged refusal reported ${String(forged)}`);
    check('auth.forged-refusal.parked', !(await victim.node.client.hasPendingWork())
      ? '' : 'a forged refusal left the loop retrying');
  } finally {
    globalThis.fetch = realFetch;
  }
  // The backoff expires and the statement is delivered: a forged refusal delays convergence
  // but cannot prevent it, and the peer had nothing to send in the meantime.
  clock.value += REFUSED_RETRY_MS + 1;
  await victim.node.trust.redrive();
  check('auth.forged-refusal.recovered', trustPhase((await forgedPeer.node.trust.entry('urn:onlyne:agent:victim'))!) === 'invited'
    && !(await victim.node.client.hasPendingWork())
    ? '' : 'the approval never reached the peer after the backoff');

  // A rotation with no peers must not leave a window behind: the next rotation would be
  // refused forever.
  const lonely = serve('urn:onlyne:agent:lonely', base_url => keyed('urn:onlyne:agent:lonely', base_url));
  const lonelyFirst = await lonely.node.trust.rotate({ actor: 'urn:onlyne:agent:lonely' });
  const lonelySecond = await lonely.node.trust.rotate({ actor: 'urn:onlyne:agent:lonely' });
  check('auth.rotate.no-peers', lonelyFirst.fingerprint !== lonelySecond.fingerprint
    ? '' : 'a second rotation with no peers was refused or reused the key');

  // Revoking the last peer of an unacknowledged rotation closes its window: otherwise every
  // later rotation would be refused by a window nothing can settle.
  const windowed = serve('urn:onlyne:agent:windowed', base_url => keyed('urn:onlyne:agent:windowed', base_url));
  const gonePeer = serve('urn:onlyne:agent:gone', base_url => keyed('urn:onlyne:agent:gone', base_url));
  await windowed.node.trust.request({ actor: 'urn:onlyne:agent:windowed' }, 'urn:onlyne:agent:gone', (await gonePeer.node.keys.describe())!.fingerprint);
  await gonePeer.node.trust.request({ actor: 'urn:onlyne:agent:gone' }, 'urn:onlyne:agent:windowed', (await windowed.node.keys.describe())!.fingerprint);
  dropIntroductions = true;
  globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit) => {
    if (String(input).includes('/introductions')) throw new OnlyneError('unavailable', 'the rotation statement never left this node');
    return realFetch(input, init);
  }) as typeof fetch;
  try {
    await windowed.node.trust.rotate({ actor: 'urn:onlyne:agent:windowed' });
  } finally {
    globalThis.fetch = realFetch;
    dropIntroductions = false;
  }
  await windowed.node.trust.revoke({ actor: 'urn:onlyne:agent:windowed' }, 'urn:onlyne:agent:gone');
  check('auth.rotate.window-closed', await succeeds(() => windowed.node.trust.rotate({ actor: 'urn:onlyne:agent:windowed' })));

  // A peer that already recorded this node's key owes the rotation proof even before this node
  // approved it: otherwise the approval would arrive under a key the peer cannot prove.
  const inviting = serve('urn:onlyne:agent:inviting', base_url => keyed('urn:onlyne:agent:inviting', base_url));
  const invited = serve('urn:onlyne:agent:invited', base_url => keyed('urn:onlyne:agent:invited', base_url));
  await invited.node.trust.request({ actor: 'urn:onlyne:agent:invited' }, 'urn:onlyne:agent:inviting', (await inviting.node.keys.describe())!.fingerprint);
  check('auth.rotate.invited-state', trustPhase((await inviting.node.trust.entry('urn:onlyne:agent:invited'))!) === 'invited'
    ? '' : 'the invited row did not form');
  const invitingRotated = await inviting.node.trust.rotate({ actor: 'urn:onlyne:agent:inviting' });
  await inviting.node.trust.accept({ actor: 'urn:onlyne:agent:inviting' }, 'urn:onlyne:agent:invited', (await invited.node.keys.describe())!.fingerprint);
  const invitedView = await invited.node.trust.entry('urn:onlyne:agent:inviting');
  check('auth.rotate.invited-proof', invitedView?.fingerprint === invitingRotated.fingerprint
    && trustPhase(invitedView) === 'exchanged'
    ? '' : `the peer read ${invitedView?.fingerprint}/${invitedView === undefined ? 'none' : trustPhase(invitedView)}`);

  // A host key hook slower than one retry window must not let the loop abandon the swap: this
  // process is still inside it, so the proof is not a crash leftover.
  const slowGate = Promise.withResolvers<void>();
  const slow = serve('urn:onlyne:agent:slow', base_url => keyed('urn:onlyne:agent:slow', base_url, [], {}, () => clock.value, slowGate.promise));
  const patient = serve('urn:onlyne:agent:patient', base_url => keyed('urn:onlyne:agent:patient', base_url));
  await slow.node.trust.request({ actor: 'urn:onlyne:agent:slow' }, 'urn:onlyne:agent:patient', (await patient.node.keys.describe())!.fingerprint);
  await patient.node.trust.request({ actor: 'urn:onlyne:agent:patient' }, 'urn:onlyne:agent:slow', (await slow.node.keys.describe())!.fingerprint);
  const slowRotation = slow.node.trust.rotate({ actor: 'urn:onlyne:agent:slow' });
  await sleep(20);
  clock.value += EXCHANGE_RETRY_MS + 1;
  await slow.node.trust.redrive();
  check('auth.rotate.slow-hook-retained', await slow.node.client.hasPendingWork()
    ? '' : 'a slow key hook let the loop abandon the rotation');
  slowGate.resolve();
  const slowIdentity = await slowRotation;
  clock.value += EXCHANGE_RETRY_MS + 1;
  await slow.node.trust.redrive();
  check('auth.rotate.slow-hook-converged', (await patient.node.trust.entry('urn:onlyne:agent:slow'))?.fingerprint === slowIdentity.fingerprint
    ? '' : 'the peer never learned the rotated key');

  // A host without a maintenance clock schedules `maintain` itself. That pass must re-drive owed
  // statements, or the approval is owed forever with no legal call left to deliver it.
  const clockless = serve('urn:onlyne:agent:clockless', base_url => keyed('urn:onlyne:agent:clockless', base_url));
  const clocklessPeer = serve('urn:onlyne:agent:clockless-peer', base_url => keyed('urn:onlyne:agent:clockless-peer', base_url));
  globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit) => {
    if (String(input).includes('/introductions')) throw new OnlyneError('unavailable', 'the introduction never left this node');
    return realFetch(input, init);
  }) as typeof fetch;
  try {
    await clockless.node.trust.request({ actor: 'urn:onlyne:agent:clockless' }, 'urn:onlyne:agent:clockless-peer', (await clocklessPeer.node.keys.describe())!.fingerprint)
      .catch(() => undefined);
  } finally {
    globalThis.fetch = realFetch;
  }
  check('auth.noclock.owed', await clockless.node.client.hasPendingWork()
    ? '' : 'a failed introduction is not owed work without a clock');
  clock.value += EXCHANGE_RETRY_MS + 1;
  await clockless.node.client.maintain();
  check('auth.noclock.maintain-redrives', trustPhase((await clocklessPeer.node.trust.entry('urn:onlyne:agent:clockless'))!) === 'invited'
    && !(await clockless.node.client.hasPendingWork())
    ? '' : 'maintain() did not deliver the owed statement');

  // A key hook that swaps and then throws must not lose the rotation proof: the library re-reads
  // the hook, sees the new key, and keeps the window so the peers can still be told.
  const faulty = serve('urn:onlyne:agent:faulty', base_url => keyed('urn:onlyne:agent:faulty', base_url, [], {}, () => clock.value, undefined, 'after-swap'));
  const steady = serve('urn:onlyne:agent:steady', base_url => keyed('urn:onlyne:agent:steady', base_url));
  await faulty.node.trust.request({ actor: 'urn:onlyne:agent:faulty' }, 'urn:onlyne:agent:steady', (await steady.node.keys.describe())!.fingerprint);
  await steady.node.trust.request({ actor: 'urn:onlyne:agent:steady' }, 'urn:onlyne:agent:faulty', (await faulty.node.keys.describe())!.fingerprint);
  const failedRotation = await faulty.node.trust.rotate({ actor: 'urn:onlyne:agent:faulty' }).catch(error => error as Error);
  check('auth.rotate.hook-failed', failedRotation instanceof Error ? '' : 'a failing key hook reported success');
  const swappedIdentity = (await faulty.node.keys.describe())!;
  clock.value += EXCHANGE_RETRY_MS + 1;
  await faulty.node.trust.redrive();
  check('auth.rotate.hook-failed-converged', (await steady.node.trust.entry('urn:onlyne:agent:faulty'))?.fingerprint === swappedIdentity.fingerprint
    ? '' : 'the rotation window was abandoned after the hook had already swapped the key');


}

// ---- run --------------------------------------------------------------------

/**
 * RFC 0017 §6: the loop owns its cadence through one-shot timers. A pass must leave
 * exactly one armed timer, and stopping must leave none — a fired handle dropped without
 * being cleared leaks one timer per pass and makes `stopMaintenance` incomplete.
 */
async function phaseMaintenance(): Promise<void> {
  const live = new Map<number, () => void>();
  let seq = 0;
  const clock = {
    setTimer: (_ms: number, run: () => void): unknown => { const id = ++seq; live.set(id, run); return id; },
    clearTimer: (handle: unknown): void => { live.delete(handle as number); },
  };
  const loop = rig('urn:onlyne:agent:loopy', 'http://127.0.0.1:9/onlyne/v3', { maintenanceClock: clock });
  loop.node.client.runMaintenance();
  check('maintenance.armed', live.size === 1 ? '' : `runMaintenance armed ${live.size} timers`);
  // This clock keeps a fired timer registered until it is cleared, which is where a pass
  // that drops the spent handle instead of clearing it leaks one timer per cadence.
  const fire = [...live.values()][0]!;
  await fire();
  check('maintenance.rearmed', live.size === 1 ? '' : `one pass left ${live.size} timers armed`);
  loop.node.client.stopMaintenance();
  check('maintenance.stopped', live.size === 0 ? '' : `stopMaintenance left ${live.size} timers armed`);
}

try {
  await phaseLocal();
  await phaseNet();
  await phaseRecoverOutbound();
  await phaseExpiry();
  await phaseResources();
  await phaseAuth();
  await phaseMaintenance();
} catch (error) {
  const problem = `unexpected error: ${text(error)}`;
  failures.push(problem);
  console.log(`FAIL ${problem}`);
} finally {
  shutdown();
  console.log(failures.length === 0 ? '\n全部通过' : `\n${failures.length} 项失败`);
  if (failures.length > 0) process.exit(1);
}
