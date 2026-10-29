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
import type { SendOutcome } from '../src/index.js';
import { canonical, key } from '../src/core.js';
import { createPrivateKey, sign } from 'node:crypto';
import type {
  Auth, DeliveryPlan, Entry, ErrorCode, HandoverProbe, Json, KeyMaterial, Mutation, NodeOptions, Plugins, Resource,
  ResourceDriver, ResourceRef, SendRequest, Target, ThreadRef, TrustEntry, TrustSeed,
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

/** One field of a wire JSON value, read only when the value really carries it. */
function field(value: Json | undefined, name: string): unknown {
  return typeof value === 'object' && value !== null && !Array.isArray(value) && name in value ? value[name] : undefined;
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

function rig(node_id: string, base_url: string, extra: Partial<Plugins> = {}, wait_ms = 5_000): Rig {
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
  const options: NodeOptions = { node_id, base_url, plugins, now: () => clock.value, id: () => crypto.randomUUID(), wait_ms };
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
  // The receipt is the only place the sender can prove what it answered (cyanea report 2).
  check('messages.reply.receipt', reply.in_reply_to?.sender === send.message.sender && reply.in_reply_to.id === send.message.id
    ? '' : `the receipt lost the answered reference: ${JSON.stringify(reply.in_reply_to)}`);

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

  // One exchange reads back as one conversation with both directions (dpdp report 4).
  const conversation = (await lazuli.node.history.list(lazuliAuth)).items
    .find(row => row.messages.some(message => message.message_ref.id === 'msg-01'));
  check('history.conversation', conversation !== undefined
    && conversation.messages.some(row => row.direction === 'in' && row.message_ref.id === 'msg-01')
    && conversation.messages.some(row => row.direction === 'out' && row.message_ref.id === 'msg-02')
    ? '' : `the exchange did not read back as one conversation: ${JSON.stringify(conversation?.messages.map(row => `${row.direction}:${row.message_ref.id}`))}`);
  const single = await lazuli.node.history.message(lazuliAuth, { sender: dpdpNode, id: 'msg-01' });
  check('history.message', single.direction === 'in' && single.thread_ref?.id === thread.thread_ref.id && single.consumption !== undefined
    ? '' : `unexpected single message ${JSON.stringify(single)}`);

  // A reply whose original landing is closed or unknown refuses instead of relocating.
  await lazuli.node.threads.close(lazuliAuth, thread.thread_ref, mut());
  check('messages.reply.closed', await refusal('thread_closed', () => lazuli.node.messages.admit(lazuliAuth, {
    idempotency_key: crypto.randomUUID(), retry_until: future(),
    message: { id: 'msg-04', sender: lazuliNode, delivery_kind: '迟到回复', msg: '这条不该换线程。', meta: { in_reply_to: { sender: dpdpNode, id: 'msg-01' }, relation: 'reply' } },
    target_endpoint: lazuliNode, plan,
  })));
  check('messages.reply.unknown', await refusal('not_found', () => lazuli.node.messages.admit(lazuliAuth, {
    idempotency_key: crypto.randomUUID(), retry_until: future(),
    message: { id: 'msg-05', sender: lazuliNode, delivery_kind: '未知原信', msg: '没有可续写的落点。', meta: { in_reply_to: { sender: dpdpNode, id: 'never-arrived' }, relation: 'reply' } },
    target_endpoint: lazuliNode, plan,
  })));
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
// 3. joined threads: a member lists and reads a thread another node hosts
// =============================================================================
/**
 * A thread one node hosts and another was invited into. The member must be able to list it,
 * read its owner-held facts and read its entries; a thread it was never invited into must
 * stay absent, and an owner that cannot be reached must leave the row in place with the
 * failure attached instead of hiding the thread.
 */
async function phaseJoined(): Promise<void> {
  let ownerHandler: (request: Request) => Promise<Response> = async () => new Response('starting', { status: 503 });
  const ownerServer = Bun.serve({ port: 0, hostname: '127.0.0.1', fetch: request => ownerHandler(request) });
  servers.push(ownerServer);
  urls[lazuliNode] = `http://127.0.0.1:${ownerServer.port}/onlyne/v3`;
  const ownerRig = rig(lazuliNode, urls[lazuliNode]!, { mayAdmit: async () => true });
  ownerHandler = request => ownerRig.node.fetch(request);
  const memberRig = serve(dpdpNode, base_url => rig(dpdpNode, base_url, { mayAdmit: async () => true }));
  const owner = ownerRig.node;
  const member = memberRig.node;
  const authOwner: Auth = { actor: lazuliNode };
  const authMember: Auth = { actor: dpdpNode };

  const thread = await owner.threads.create(authOwner, { ...mut(), title: 'joined review' });
  const ref = thread.thread_ref;
  await owner.threads.member(authOwner, ref, dpdpNode, { access: ['post', 'read'] }, mut());
  for (const [id, msg] of [['join-01', '第一段。'], ['join-02', '第二段。']] as const) {
    await owner.messages.admit(authOwner, {
      idempotency_key: crypto.randomUUID(), retry_until: future(),
      message: { id, sender: lazuliNode, delivery_kind: '进度', msg, meta: { thread: ref, mentions: ['@all'] } },
      target_endpoint: lazuliNode, plan,
    });
  }
  // The owner pushes its grant notice, then the member pulls both obligations.
  await owner.client.maintain();
  await member.client.reconcile();

  const local = await member.threads.joined(authMember);
  const row = local.items.find(item => item.thread_ref.id === ref.id);
  check('joined.local_listing', row !== undefined && row.thread_ref.owner === lazuliNode
    && row.last_seq === '2' && row.message_count === 2 && row.read_only === true
    ? '' : `the member listed ${JSON.stringify(row)}`);

  const described = await member.client.joinedThreads();
  const withOwner = described.items.find(item => item.thread_ref.id === ref.id);
  check('joined.owner_facts', withOwner?.title === 'joined review' && withOwner.state === 'open'
    && withOwner.unavailable === undefined
    && withOwner.members?.some(entry => entry.peer === dpdpNode && entry.state === 'active') === true
    ? '' : `the owner did not answer the describe read: ${JSON.stringify(withOwner)}`);

  // The member holds no copy of the entries, so reading them is the owner's answer.
  const entries = await member.client.threadEntries(ref, { attention: 'thread' });
  check('joined.messages', entries.items.length === 2 && entries.items[1]?.message.msg === '第二段。'
    ? '' : `the member read ${entries.items.length} entries from the owner`);

  // The HTTP view is the same listing, and only the node itself may read it.
  const own = await fetch(`${urls[dpdpNode]}/threads?joined=1&page_size=10`, { headers: { 'x-onlyne-actor': dpdpNode } });
  const listed = await own.json() as { items?: Array<{ thread_ref?: ThreadRef }> };
  check('joined.http', own.status === 200 && listed.items?.some(item => item.thread_ref?.id === ref.id) === true
    ? '' : `GET /threads?joined=1 answered ${own.status}`);
  const foreign = await fetch(`${urls[dpdpNode]}/threads?joined=1`, { headers: { 'x-onlyne-actor': lazuliNode } });
  check('joined.http.actor', foreign.status === 403
    ? '' : `another actor read the joined view with ${foreign.status}`);

  const uninvited = await owner.threads.create(authOwner, { ...mut(), title: 'owner only' });
  const absent = await member.threads.joined(authMember);
  check('joined.uninvited_absent', absent.items.some(item => item.thread_ref.id === uninvited.thread_ref.id) === false
    ? '' : 'a thread the member was never invited into appeared in its joined listing');

  // The owner goes away: the row must survive with the failure on it, not vanish.
  ownerServer.stop(true);
  const offline = await member.client.joinedThreads();
  const marked = offline.items.find(item => item.thread_ref.id === ref.id);
  check('joined.owner_unavailable', marked !== undefined && marked.unavailable?.code === 'unavailable' && marked.last_seq === '2'
    ? '' : `an unreachable owner answered ${JSON.stringify(marked)}`);
}

// =============================================================================
// 4. outbound recovery: a pending delivery re-driven after the peer comes back
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
// 5. resources: binding, append-only log, cursor ack, probe handover, recovery
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

  // The target answers from its own policy. The flags let one scenario refuse a probe, and one
  // write an event while the endpoint is frozen — exactly the window the handshake must cover.
  const probe = { refuse: false, during: undefined as undefined | (() => Promise<void>), seen: undefined as Json | undefined };
  const keeper = serve(keeperNode, base_url => rig(keeperNode, base_url, {
    resources: driver,
    // One peer is unreachable: its handover must abort without moving the binding.
    resolve: async (node: string): Promise<Target> => (node === caeruliaNode
      ? { node_id: node, base_url: 'http://127.0.0.1:1/onlyne/v3' }
      : { node_id: node, base_url: urls[node] ?? base_url, headers: { 'x-onlyne-actor': keeperNode } }),
  }));
  const target = serve(dpdpNode, base_url => rig(dpdpNode, base_url, {
    handoverProbe: async value => {
      probe.seen = value.metadata;
      if (probe.during !== undefined) await probe.during();
      return probe.refuse
        ? { answer: 'reject', reason: 'capability_mismatch' }
        : { answer: 'accept', cursor: value.cursor };
    },
  }));
  const node = keeper.node;
  const authK: Auth = { actor: keeperNode };
  const authD: Auth = { actor: dpdpNode };
  const authC: Auth = { actor: caeruliaNode };

  const resource: Resource = {
    resource_ref: { owner: keeperNode, id: 'chat-01' }, kind: 'im_conversation',
    operations: ['read', 'send', 'list_events', 'describe', 'handover'],
    binding: { handler: keeperNode, cursor: '0' },
    log_readers: [keeperNode, dpdpNode],
    event_seq: '0', revision: 'rev-a', retention_ms: 60_000,
  };
  const ref = resource.resource_ref;
  const registered = await node.resources.register(authK, resource);
  check('resources.register', registered.revision === 'rev-a' && registered.binding.handler === keeperNode
    ? '' : `the resource did not register: ${JSON.stringify(registered)}`);

  const event = await node.resources.receive(authK, ref, { source_id: 'platform-1', data: { text: '图里的单位是什么' } });
  check('resources.receive', event.event_id === '1'
    && Object.keys(event).sort().join(',') === 'at,data,event_id,expired,expires_at,resource_ref,source_id'
    ? '' : `unexpected event ${JSON.stringify(event)}`);
  const duplicate = await node.resources.receive(authK, ref, { source_id: 'platform-1', data: { text: '重复' } });
  check('resources.receive.deduplicate', duplicate.event_id === '1' ? '' : `a duplicate source got ${duplicate.event_id}`);
  await node.resources.receive(authK, ref, { source_id: 'platform-2', data: { text: '第二条' } });

  // RFC 0006 §1: the query operations answer from the endpoint's own state, never the driver.
  const queryRead = await node.resources.operate(authK, ref, {
    ...mut(), operation_id: crypto.randomUUID(), operation: 'read', data: { event_id: '1' },
  });
  check('resources.query.read', queryRead.state === 'committed'
    && field(queryRead.data, 'event_id') === '1'
    ? '' : `read answered ${JSON.stringify(queryRead)}`);
  const queryList = await node.resources.operate(authK, ref, {
    ...mut(), operation_id: crypto.randomUUID(), operation: 'list_events', data: { page_size: 10 },
  });
  const listed = field(queryList.data, 'items');
  check('resources.query.list_events', queryList.state === 'committed'
    && Array.isArray(listed) && listed.length === 2
    ? '' : `list_events answered ${JSON.stringify(queryList)}`);
  const queryDescribe = await node.resources.operate(authK, ref, {
    ...mut(), operation_id: crypto.randomUUID(), operation: 'describe',
  });
  check('resources.query.describe', queryDescribe.state === 'committed'
    && field(queryDescribe.data, 'revision') === 'rev-a'
    ? '' : `describe answered ${JSON.stringify(queryDescribe)}`);
  const queryRecord = await node.resources.operation(authK, ref, queryRead.operation_id)
    .then(() => 'recorded', () => 'absent');
  check('resources.query.no_record', queryRecord === 'absent' ? '' : 'a query wrote an operation record');
  check('resources.query.bad_read', await refusal('invalid_request', () => node.resources.operate(authK, ref, {
    ...mut(), operation_id: crypto.randomUUID(), operation: 'read',
  })));

  // RFC 0006 §6: the log is readable by the handler and by the standing readers, nobody else.
  const handlerLog = await node.resources.log(authK, ref, {});
  check('resources.log.handler', handlerLog.items.length === 2 ? '' : `the handler read ${handlerLog.items.length} events`);
  const readerLog = await node.resources.log(authD, ref, {});
  check('resources.log.reader', readerLog.items.length === 2 ? '' : `a log reader read ${readerLog.items.length} events`);
  check('resources.log.stranger', await refusal('not_found', () => node.resources.log(authC, ref, {})));
  const sinceLog = await node.resources.log(authK, ref, { since: '1' });
  check('resources.log.since', sinceLog.items.map(item => item.event_id).join(',') === '2'
    ? '' : `since=1 returned ${JSON.stringify(sinceLog.items.map(item => item.event_id))}`);
  const single = await node.resources.event(authD, ref, '1');
  check('resources.log.event', single.event_id === '1' && single.source_id === 'platform-1'
    ? '' : `a single log read answered ${JSON.stringify(single)}`);
  check('resources.log.unknown_event', await refusal('not_found', () => node.resources.event(authK, ref, '9')));

  // RFC 0006 §5: an ack moves the handler's cursor forward and nothing else.
  const acked = await node.resources.ack(authK, ref, '1', mut());
  check('resources.ack.cursor', acked.cursor === '1' && acked.resource_ref.id === 'chat-01'
    ? '' : `the ack answered ${JSON.stringify(acked)}`);
  const ackedAgain = await node.resources.ack(authK, ref, '2', mut());
  const behind = await node.resources.ack(authK, ref, '1', mut());
  check('resources.ack.forward', ackedAgain.cursor === '2' && behind.cursor === '2'
    ? '' : `the cursor went ${ackedAgain.cursor} then ${behind.cursor}`);
  check('resources.ack.reader_forbidden', await refusal('forbidden', () => node.resources.ack(authD, ref, '2', mut())));
  check('resources.ack.unknown_event', await refusal('not_found', () => node.resources.ack(authK, ref, '9', mut())));

  // One operation id executes once; the replay reads back the recorded result.
  const request = { ...mut(), operation_id: crypto.randomUUID(), operation: 'send' };
  const first = await node.resources.operate(authK, ref, request);
  const second = await node.resources.operate(authK, ref, request);
  const runs = executions.get(request.operation_id) ?? 0;
  check('resources.operation.replay', first.state === 'committed' && second.state === 'committed'
    && JSON.stringify(second.data) === JSON.stringify(first.data) && runs === 1
    ? '' : `replay executed ${runs} times and reported ${first.state}/${second.state}`);
  const record = await node.resources.operation(authK, ref, request.operation_id);
  check('resources.operation.record', record.result.state === 'committed' && record.actor === keeperNode
    ? '' : `the operation record reported ${record.result.state}`);
  const raced = { ...mut(), operation_id: crypto.randomUUID(), operation: 'send' };
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

  // RFC 0006 §4.2/§4.4: an unacked event does not block a handover, and an event written while
  // the endpoint is frozen stays behind the frozen cursor, so the new handler inherits it.
  const pending = await node.resources.receive(authK, ref, { source_id: 'platform-3', data: { text: '交接前未确认' } });
  check('resources.handover.unacked', pending.event_id === '3' ? '' : 'the unacked event was not appended');
  probe.during = async () => { await node.resources.receive(authK, ref, { source_id: 'freeze-1', data: { text: '冻结期到达' } }); };
  const handoverId = crypto.randomUUID();
  const deadline = future();
  const committed = await node.resources.operate(authK, ref, {
    ...mut(), operation_id: handoverId, operation: 'handover', expected_revision: 'rev-a',
    data: { target: dpdpNode, handover_deadline: deadline, metadata: { reason: '转交技术专家' } },
  });
  probe.during = undefined;
  check('resources.handover.commit', committed.state === 'committed' ? '' : `the handover reported ${JSON.stringify(committed)}`);
  check('resources.handover.metadata', JSON.stringify(probe.seen) === JSON.stringify({ reason: '转交技术专家' })
    ? '' : `the target read ${JSON.stringify(probe.seen)}`);
  const moved = await node.resources.describe(authD, ref);
  check('resources.handover.handler', moved.binding.handler === dpdpNode ? '' : 'the binding did not move');
  check('resources.handover.revision', moved.revision !== 'rev-a' ? '' : 'the commit did not move the revision');
  check('resources.freeze.success', moved.binding.cursor === '2'
    ? '' : `the new handler starts at ${moved.binding.cursor} instead of the frozen position`);
  const inherited = await node.resources.log(authD, ref, { since: moved.binding.cursor });
  check('resources.freeze.inherited', inherited.items.map(item => item.event_id).join(',') === '3,4'
    ? '' : `the new handler sees ${inherited.items.map(item => item.event_id).join(',')}`);

  // RFC 0006 §7: the revision is the send fence, so the former handler learns the binding moved.
  check('resources.fence.old_handler', await refusal('revision_conflict', () => node.resources.operate(authK, ref, {
    ...mut(), operation_id: crypto.randomUUID(), operation: 'send', expected_revision: 'rev-a', data: { text: '旧版本发送' },
  })));
  const sent = await node.resources.operate(authD, ref, {
    ...mut(), operation_id: crypto.randomUUID(), operation: 'send', expected_revision: moved.revision, data: { text: '新处理者发送' },
  });
  check('resources.handover.new_handler_sends', sent.state === 'committed' ? '' : `the new handler's send answered ${sent.state}`);

  // RFC 0006 §6: the switch never edits log_readers. The former handler keeps its history only
  // while the table names it; the handler itself reads without being in the table.
  const retained = await node.resources.log(authK, ref, {});
  check('resources.log.former_handler', retained.items.length === 4 ? '' : `the former handler read ${retained.items.length} events`);
  await keeper.storage.transaction(async tx => {
    const stored = await tx.get<Resource>(key('resource', keeperNode, 'chat-01'));
    await tx.put(key('resource', keeperNode, 'chat-01'), { ...stored!, log_readers: [caeruliaNode] } satisfies Resource);
  });
  check('resources.log.reader_removed', await refusal('not_found', () => node.resources.log(authK, ref, {})));
  const detached = await node.resources.log(authC, ref, {});
  check('resources.log.detached', detached.items.length === 4
    ? '' : `a log reader outside the binding read ${detached.items.length} events`);
  const handlerStill = await node.resources.log(authD, ref, {});
  check('resources.log.handler_not_reader', handlerStill.items.length === 4
    ? '' : 'the handler lost its own log while not being a reader');

  // RFC 0006 §4.5: the same operation id answers with its original result; a different target or
  // metadata under that id is another request.
  const replay = await node.resources.operate(authK, ref, {
    ...mut(), operation_id: handoverId, operation: 'handover', expected_revision: 'rev-a',
    data: { target: dpdpNode, handover_deadline: deadline, metadata: { reason: '转交技术专家' } },
  });
  check('resources.handover.replay', replay.state === 'committed'
    && JSON.stringify(replay.data) === JSON.stringify(committed.data)
    ? '' : `the replay answered ${JSON.stringify(replay)}`);
  check('resources.handover.id_conflict', await refusal('id_conflict', () => node.resources.operate(authK, ref, {
    ...mut(), operation_id: handoverId, operation: 'handover', expected_revision: 'rev-a',
    data: { target: keeperNode, handover_deadline: deadline, metadata: { reason: '转交技术专家' } },
  })));

  // RFC 0006 §4.5: two handovers on one revision — only the first switches, the other is told the
  // binding moved and reports the current revision.
  const raceDeadline = future();
  const race = (id: string) => node.resources.operate(authD, ref, {
    ...mut(), operation_id: id, operation: 'handover', expected_revision: moved.revision,
    data: { target: keeperNode, handover_deadline: raceDeadline },
  });
  const [firstRace, secondRace] = await Promise.all([race(crypto.randomUUID()), race(crypto.randomUUID())]);
  const outcomes = [firstRace.state, secondRace.state].sort().join(',');
  const loser = [firstRace, secondRace].find(result => result.state === 'aborted');
  check('resources.handover.race', outcomes === 'aborted,committed'
    && loser?.error?.code === 'revision_conflict'
    ? '' : `two handovers on one revision settled as ${outcomes}/${JSON.stringify(loser?.error)}`);
  const racedBinding = await node.resources.describe(authK, ref);
  check('resources.handover.race_binding', racedBinding.binding.handler === keeperNode
    ? '' : 'the race left another handler in place');

  // RFC 0006 §4.3: a refusal and an unreachable target both leave the binding and revision alone,
  // and the event written during the freeze stays owed to the original handler.
  const failingRef: ResourceRef = { owner: keeperNode, id: 'chat-02' };
  const failingRevision = 'rev-b';
  await node.resources.register(authK, {
    resource_ref: failingRef, kind: 'im_conversation',
    operations: ['read', 'send', 'list_events', 'describe', 'handover'],
    binding: { handler: keeperNode, cursor: '0' },
    log_readers: [keeperNode],
    event_seq: '0', revision: failingRevision, retention_ms: 60_000,
  });
  await node.resources.receive(authK, failingRef, { source_id: 'p-1', data: { text: '第一条' } });
  await node.resources.ack(authK, failingRef, '1', mut());
  probe.refuse = true;
  probe.during = async () => { await node.resources.receive(authK, failingRef, { source_id: 'freeze-2', data: { text: '冻结期到达' } }); };
  const rejectedId = crypto.randomUUID();
  const rejected = await node.resources.operate(authK, failingRef, {
    ...mut(), operation_id: rejectedId, operation: 'handover', expected_revision: failingRevision,
    data: { target: dpdpNode, handover_deadline: future() },
  });
  probe.during = undefined;
  probe.refuse = false;
  check('resources.handover.rejected', rejected.state === 'aborted'
    && field(rejected.data, 'reason') === 'capability_mismatch'
    ? '' : `a refused probe answered ${JSON.stringify(rejected)}`);
  const rejectedRecord = await node.resources.operation(authK, failingRef, rejectedId);
  check('resources.handover.rejected_record', rejectedRecord.result.state === 'aborted'
    ? '' : `the refused handover recorded ${rejectedRecord.result.state}`);
  const unmoved = await node.resources.describe(authK, failingRef);
  check('resources.freeze.abort', unmoved.binding.handler === keeperNode && unmoved.revision === failingRevision
    && unmoved.binding.cursor === '1'
    ? '' : `a failed handover moved the binding: ${JSON.stringify(unmoved)}`);
  const owed = await node.resources.log(authK, failingRef, { since: unmoved.binding.cursor });
  check('resources.freeze.returned', owed.items.map(item => item.event_id).join(',') === '2'
    ? '' : `the original handler sees ${owed.items.map(item => item.event_id).join(',')}`);

  const unreachable = await node.resources.operate(authK, failingRef, {
    ...mut(), operation_id: crypto.randomUUID(), operation: 'handover', expected_revision: failingRevision,
    data: { target: caeruliaNode, handover_deadline: future() },
  });
  check('resources.handover.unreachable', unreachable.state === 'aborted'
    && field(unreachable.data, 'reason') === 'unreachable'
    ? '' : `an unreachable target answered ${JSON.stringify(unreachable)}`);
  const stillHeld = await node.resources.describe(authK, failingRef);
  check('resources.handover.unreachable_binding', stillHeld.binding.handler === keeperNode && stillHeld.revision === failingRevision
    ? '' : 'an unreachable target moved the binding');

  // Retention writes a terminal disposition onto the event; the id is never reused.
  const expiring = await node.resources.receive(authK, failingRef, {
    source_id: 'p-expire', data: { text: '过期' }, expires_at: new Date(clock.value - 1_000).toISOString(),
  });
  await node.resources.recover();
  const swept = await node.resources.event(authK, failingRef, expiring.event_id);
  check('resources.recover.expiry', swept.expired === true
    ? '' : `the retention sweep left event ${expiring.event_id} unexpired`);

  // RFC 0006 §4.3: the target records one answer per operation id and refuses a late probe.
  const probeId = crypto.randomUUID();
  const probeShape: HandoverProbe = {
    operation_id: probeId, resource: failingRef, kind: 'im_conversation',
    operations: ['read', 'send'], metadata: { reason: '测试' }, initiator: keeperNode,
    cursor: '1', deadline: future(),
  };
  const answered = await target.node.resources.probe(authK, probeShape);
  check('resources.probe.accept', answered.answer === 'accept' && answered.cursor === '1'
    ? '' : `the target answered ${JSON.stringify(answered)}`);
  const answeredAgain = await target.node.resources.probe(authK, probeShape);
  check('resources.probe.replay', JSON.stringify(answeredAgain) === JSON.stringify(answered)
    ? '' : `a probe replay answered ${JSON.stringify(answeredAgain)}`);
  check('resources.probe.id_conflict', await refusal('id_conflict', () => target.node.resources.probe(authK, {
    ...probeShape, metadata: { reason: '改了' },
  })));
  const late = await target.node.resources.probe(authK, {
    ...probeShape, operation_id: crypto.randomUUID(), deadline: new Date(clock.value - 1_000).toISOString(),
  });
  check('resources.probe.deadline', late.answer === 'reject' && late.reason === 'deadline_passed'
    ? '' : `a late probe answered ${JSON.stringify(late)}`);
  const stored = await target.node.resources.probeAnswer(authK, probeId);
  check('resources.probe.answer', JSON.stringify(stored) === JSON.stringify(answered)
    ? '' : `the recorded answer read back as ${JSON.stringify(stored)}`);
}

// =============================================================================
// 5b. handover recovery: a probe past the local wait settles from the target's record
// =============================================================================
async function phaseHandoverRecovery(): Promise<void> {
  const driver: ResourceDriver = { validate: async () => undefined, execute: async () => ({ state: 'committed' as const }) };
  const owner = serve(keeperNode, base_url => rig(keeperNode, base_url, { resources: driver }, 50));
  serve(dpdpNode, base_url => rig(dpdpNode, base_url, {
    handoverProbe: async value => {
      // The target answers after the endpoint's local wait has already expired.
      await sleep(300);
      return { answer: 'accept', cursor: value.cursor };
    },
  }));
  const authK: Auth = { actor: keeperNode };
  const ref: ResourceRef = { owner: keeperNode, id: 'chat-slow' };
  await owner.node.resources.register(authK, {
    resource_ref: ref, kind: 'im_conversation', operations: ['read', 'send', 'handover'],
    binding: { handler: keeperNode, cursor: '0' }, log_readers: [keeperNode],
    event_seq: '0', revision: 'rev-slow', retention_ms: 60_000,
  });
  const pending = await owner.node.resources.operate(authK, ref, {
    ...mut(), operation_id: crypto.randomUUID(), operation: 'handover', expected_revision: 'rev-slow',
    data: { target: dpdpNode, handover_deadline: future(5_000) },
  });
  check('resources.handover.unknown', pending.state === 'unknown'
    ? '' : `a slow probe reported ${pending.state} instead of unknown`);
  await sleep(400);
  await owner.node.resources.recover();
  const moved = await owner.node.resources.describe(authK, ref);
  check('resources.handover.recovered', moved.binding.handler === dpdpNode && moved.revision !== 'rev-slow'
    ? '' : `recovery left ${JSON.stringify(moved)}`);
}


// =============================================================================
// 5c. projection settlement: an idle queued projection resolves once the resource has moved
// =============================================================================
/**
 * The reported defect: a resource projection whose inject answered `consumed: false` in an idle
 * session stayed `injected: false`, and every maintenance pass re-injected it — even after the
 * resource had been handed over, when the former handler could never act on it again. Settlement
 * is the third outcome: the row becomes locally terminal, records why, and claims no consumption.
 */
async function phaseSettlement(): Promise<void> {
  const driver: ResourceDriver = { validate: async () => undefined, execute: async () => ({ state: 'committed' as const }) };
  const ownerRig = serve(keeperNode, base_url => rig(keeperNode, base_url, { resources: driver }));
  serve(dpdpNode, base_url => rig(dpdpNode, base_url, {
    handoverProbe: async value => ({ answer: 'accept', cursor: value.cursor }),
  }));
  // The former handler's outbound path: one flag fails the describe of a single resource, so a
  // pass still settles the other two and still reconciles at its end. Every request it makes
  // leaves authenticated as itself, which is what the owner checks a projection against.
  const describeFails = { on: false };
  const handlerTransport = new FetchTransport(async (node: string): Promise<Target> =>
    ({ node_id: node, base_url: urls[node]!, headers: { 'x-onlyne-actor': lazuliNode } }));
  const staged: { ref?: ResourceRef; revision?: string } = {};
  const sourceRig = serve(lazuliNode, base_url => {
    const created = rig(lazuliNode, base_url, {
      transport: {
        async request<T>(node: string, method: string, path: string, body?: unknown, signal?: AbortSignal): Promise<T> {
          if (describeFails.on && path.includes('proj-c')) {
            throw new OnlyneError('unavailable', 'The resource endpoint did not answer the describe.');
          }
          return handlerTransport.request<T>(node, method, path, body, signal);
        },
      },
      // The former handler starts each handover through the one agent entry, so the owner sees
      // a request authenticated as the binding's current handler.
      decide: async () => {
        const ref = staged.ref;
        const revision = staged.revision;
        if (ref === undefined || revision === undefined) throw new OnlyneError('decision_unavailable', 'No handover is staged.');
        return {
          kind: 'resource', resource: ref, operation: 'handover', expected_revision: revision,
          data: { target: dpdpNode, handover_deadline: future(120_000) },
        };
      },
    });
    // The idle session: an injection is queued, never confirmed, so nothing advances by itself.
    created.shared.allow = false;
    return created;
  });
  const authK: Auth = { actor: keeperNode };
  const authS: Auth = { actor: lazuliNode };
  const source = sourceRig.node;

  const projection = (id: string, log_readers: string[], revision: string): Resource => ({
    resource_ref: { owner: keeperNode, id }, kind: 'im_conversation',
    operations: ['read', 'send', 'list_events', 'describe', 'handover'],
    binding: { handler: lazuliNode, cursor: '0' }, log_readers,
    event_seq: '0', revision, retention_ms: 60_000,
  });
  const readable = projection('proj-a', [keeperNode, lazuliNode], 'rev-a1');
  const sealed = projection('proj-b', [keeperNode], 'rev-b1');
  const flaky = projection('proj-c', [keeperNode, lazuliNode], 'rev-c1');
  for (const resource of [readable, sealed, flaky]) await ownerRig.node.resources.register(authK, resource);

  const thread = await ownerRig.node.threads.create(authK, { ...mut(), title: 'resource projections' });
  const ref = thread.thread_ref;
  await ownerRig.node.threads.member(authK, ref, lazuliNode, { access: ['post', 'read'] }, mut());
  const project = (id: string, data?: { [key: string]: Json }) => ownerRig.node.messages.admit(authK, {
    idempotency_key: crypto.randomUUID(), retry_until: future(),
    message: {
      id, sender: keeperNode, delivery_kind: '资源投影', msg: `资源投影 ${id}。`,
      meta: { thread: ref, mentions: [lazuliNode], ...(data === undefined ? {} : { data }) },
    },
    target_endpoint: keeperNode, plan,
  });
  await project('proj-a-msg', { resource_ref: { ...readable.resource_ref } });
  await project('proj-b-msg', { resource_ref: { ...sealed.resource_ref } });
  await project('proj-c-msg', { resource_ref: { ...flaky.resource_ref } });
  await project('plain-1');
  // A projection naming a third party's resource is not this node's to settle: the reference is
  // trusted only when its owner is the entry's authenticated author.
  await project('foreign-1', { resource_ref: { owner: caeruliaNode, id: 'elsewhere' } });

  await ownerRig.node.client.maintain();
  await source.client.reconcile();

  const projectionIds = ['proj-a-msg', 'proj-b-msg', 'proj-c-msg'];
  const queued = (await source.messages.inbox(authS)).items;
  check('settle.queued', queued.length === 5 && queued.every(row => row.injected === false)
    ? '' : `the idle session holds ${queued.length} rows, ${queued.filter(row => row.injected).length} of them injected`);
  check('settle.queued.no_settlement', projectionIds.every(id =>
    queued.find(row => row.attention.message_ref.id === id)?.settlement === undefined)
    ? '' : 'a projection was settled while this node still held the resource');
  const queuedCounts = projectionIds.map(id => sourceRig.shared.injected.filter(entry => entry.message.id === id).length);
  check('settle.queued.injected_once', queuedCounts.every(count => count === 1)
    ? '' : `the queued projections were injected ${queuedCounts.join('/')} times`);

  const move = async (resource: ResourceRef, revision: string): Promise<string> => {
    staged.ref = resource;
    staged.revision = revision;
    const outcome = await source.client.msg({ delivery_kind: '交接资源', msg: `把 ${resource.id} 交给 dpdp。` });
    const state = field(outcome as Json, 'state');
    return typeof state === 'string' ? state : 'none';
  };
  const states = [
    await move(readable.resource_ref, 'rev-a1'),
    await move(sealed.resource_ref, 'rev-b1'),
    await move(flaky.resource_ref, 'rev-c1'),
  ];
  check('settle.handover.committed', states.every(state => state === 'committed')
    ? '' : `the handovers answered ${states.join(',')}`);
  const bindingA = await ownerRig.node.resources.describe(authK, readable.resource_ref);
  const bindingB = await ownerRig.node.resources.describe(authK, sealed.resource_ref);
  const bindingC = await ownerRig.node.resources.describe(authK, flaky.resource_ref);
  check('settle.handover.binding', [bindingA, bindingB, bindingC].every(resource => resource.binding.handler === dpdpNode)
    ? '' : 'a handover left the resource with its former handler');

  describeFails.on = true;
  const failed = await source.client.maintain().then(() => undefined, error => error as unknown);
  check('settle.describe.transient', failed instanceof OnlyneError && failed.error.code === 'unavailable'
    ? '' : `a failed describe answered ${text(failed)}`);

  const afterFailure = (await source.messages.inbox(authS)).items;
  const settledA = afterFailure.find(row => row.attention.message_ref.id === 'proj-a-msg');
  const settledB = afterFailure.find(row => row.attention.message_ref.id === 'proj-b-msg');
  const pendingC = afterFailure.find(row => row.attention.message_ref.id === 'proj-c-msg');
  const handoverSettlement = settledA?.settlement;
  check('settle.handover.row', settledA?.injected === true && handoverSettlement?.via === 'handover'
    && handoverSettlement.resource_ref.owner === keeperNode && handoverSettlement.resource_ref.id === 'proj-a'
    && handoverSettlement.handler === dpdpNode && handoverSettlement.revision === bindingA.revision
    && typeof handoverSettlement.at === 'string' && !Number.isNaN(Date.parse(handoverSettlement.at))
    ? '' : `the former handler settled as ${JSON.stringify(handoverSettlement)}`);
  const undeliverableSettlement = settledB?.settlement;
  check('settle.undeliverable.row', settledB?.injected === true && undeliverableSettlement?.via === 'undeliverable'
    && undeliverableSettlement.reason === 'resource-unavailable' && undeliverableSettlement.resource_ref.id === 'proj-b'
    ? '' : `the unreadable projection settled as ${JSON.stringify(undeliverableSettlement)}`);
  check('settle.transient.pending', pendingC?.injected === false && pendingC?.settlement === undefined
    ? '' : `a failed describe left the row as ${JSON.stringify(pendingC?.settlement)}`);
  const settledCounts = projectionIds.map(id => sourceRig.shared.injected.filter(entry => entry.message.id === id).length);
  check('settle.handover.no_reinject', settledCounts.every(count => count === 1)
    ? '' : `a settled projection was injected again: ${settledCounts.join('/')}`);

  type Consumed = { thread_ref: ThreadRef; seq: string; confirmed: boolean };
  type Owed = { entry: Entry; context: string; done: boolean };
  const consumedRows = async (): Promise<Consumed[]> =>
    sourceRig.storage.transaction(async tx => (await tx.scan<Consumed>('consumed/')).map(row => row.value));
  const owedRows = async (): Promise<Owed[]> =>
    sourceRig.storage.transaction(async tx => (await tx.scan<Owed>('todo/')).map(row => row.value));
  check('settle.no_consumption', (await consumedRows()).length === 0 ? '' : 'settling a projection wrote a consumption record');
  check('settle.no_todo', (await owedRows()).length === 0 ? '' : 'settling a projection owed a todo');

  const historyRow = await source.history.message(authS, { sender: keeperNode, id: 'proj-a-msg' });
  check('settle.history', historyRow.settlement?.via === 'handover' && historyRow.consumption !== 'consumed'
    ? '' : `history reports settlement=${JSON.stringify(historyRow.settlement)} consumption=${historyRow.consumption ?? 'none'}`);

  describeFails.on = false;
  sourceRig.shared.allow = true;
  await source.client.maintain();
  const afterPass = (await source.messages.inbox(authS)).items;
  const recovered = afterPass.find(row => row.attention.message_ref.id === 'proj-c-msg');
  const recoveredSettlement = recovered?.settlement;
  check('settle.transient.recovered', recovered?.injected === true && recoveredSettlement?.via === 'handover'
    && recoveredSettlement.resource_ref.id === 'proj-c' && recoveredSettlement.handler === dpdpNode
    && recoveredSettlement.revision === bindingC.revision
    ? '' : `the retried projection settled as ${JSON.stringify(recoveredSettlement)}`);
  const plain = afterPass.find(row => row.attention.message_ref.id === 'plain-1');
  const foreign = afterPass.find(row => row.attention.message_ref.id === 'foreign-1');
  check('settle.unrelated.injected', plain?.injected === true && foreign?.injected === true
    && plain?.settlement === undefined && foreign?.settlement === undefined
    ? '' : 'an ordinary message was settled as a projection');

  const projectionSeqs = new Set([settledA?.attention.seq, settledB?.attention.seq, recovered?.attention.seq]);
  const isProjection = (row: { thread_ref: ThreadRef; seq: string }): boolean =>
    row.thread_ref.id === ref.id && projectionSeqs.has(row.seq);
  check('settle.no_consumption.after', (await consumedRows()).every(row => !isProjection(row))
    ? '' : 'a settled projection gained a consumption record');
  check('settle.no_todo.after', (await owedRows()).every(row => !isProjection({ thread_ref: row.entry.thread_ref, seq: row.entry.seq }))
    ? '' : 'a settled projection gained a todo');
  check('settle.unrelated.consumed', (await consumedRows()).some(row => row.thread_ref.id === ref.id && row.seq === plain?.attention.seq)
    ? '' : 'the ordinary message never confirmed its consumption');

  const entryA = settledA?.attention.entry;
  if (entryA !== undefined) await source.client.confirmConsumption(entryA, 'Main');
  check('settle.late_confirm.no_slot', (await consumedRows()).every(row => !isProjection(row))
    ? '' : 'a late confirmation wrote a consumption record for a settled row');

  await source.client.reconcile();
  const reconciled = (await source.messages.inbox(authS)).items.find(row => row.attention.message_ref.id === 'proj-a-msg');
  check('settle.reconcile.preserved',
    reconciled?.injected === true && JSON.stringify(reconciled.settlement) === JSON.stringify(handoverSettlement)
      ? '' : `reconciliation rewrote the settlement as ${JSON.stringify(reconciled?.settlement)}`);
  const revived = sourceRig.recreate();
  const injectedBefore = sourceRig.shared.injected.length;
  await revived.client.maintain();
  const carried = (await revived.messages.inbox(authS)).items.find(row => row.attention.message_ref.id === 'proj-a-msg');
  check('settle.recreate.preserved',
    carried?.injected === true && JSON.stringify(carried.settlement) === JSON.stringify(handoverSettlement)
      && sourceRig.shared.injected.length === injectedBefore
      ? '' : `a fresh node over the same storage saw ${JSON.stringify(carried?.settlement)} and injected ${sourceRig.shared.injected.length - injectedBefore} more`);
  check('settle.not_owed', !(await revived.client.hasPendingWork())
    ? '' : 'a settled projection still counts as owed work');
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
  // A owns the thread it started, so the entry is stored there; B is a member and pulls the
  // obligation from the owner's authenticated attention list.
  await nodeB.client.reconcile();
  check('auth.signed_landed', (await nodeB.messages.inbox(authBOwner)).items.some(row => row.attention.message_ref.id === 'auth-01')
    ? '' : 'the signed message never reached B as a member of A\'s thread');

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

  // An invitation can arrive from a peer this node never discovered — an introduction carries its
  // own address, and discovery has no record to resolve. The accepting side must answer the
  // address that statement proved, or the row stays at `invited` with both holders approved.
  const introducedNode = 'urn:onlyne:agent:introduced';
  const introducerNode = 'urn:onlyne:agent:introducer';
  const invitedSide = serve(introducedNode, base_url => keyed(introducedNode, base_url, [], {
    resolve: async (node: string): Promise<Target> => {
      if (node === introducerNode) throw new OnlyneError('not_found', 'No discovery record names this node.');
      return { node_id: node, base_url: urls[node] ?? base_url };
    },
  }));
  const introducerSide = serve(introducerNode, base_url => keyed(introducerNode, base_url));
  const invitedIdentity = (await invitedSide.node.keys.describe())!;
  const introducerIdentity = (await introducerSide.node.keys.describe())!;
  await introducerSide.node.trust.request({ actor: introducerNode }, introducedNode, invitedIdentity.fingerprint);
  check('auth.invited.recorded', trustPhase((await invitedSide.node.trust.entry(introducerNode))!) === 'invited'
    ? '' : 'the invitation was not recorded as invited');
  const acceptedBlind = await invitedSide.node.trust.accept({ actor: introducedNode }, introducerNode, introducerIdentity.fingerprint)
    .catch(error => error as OnlyneError);
  check('auth.invited.accepted', !(acceptedBlind instanceof OnlyneError) && trustPhase(acceptedBlind as TrustEntry) === 'exchanged'
    ? '' : `accepting an introduction from an undiscovered peer reported ${String(acceptedBlind)}`);
  check('auth.invited.exchanged-both', trustPhase((await introducerSide.node.trust.entry(introducedNode))!) === 'exchanged'
    ? '' : 'the introducer never saw the approval');

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

// =============================================================================
// 7. the two facts a caller cannot read off the receipt
// =============================================================================

/**
 * A caller names an alias and the decision resolves it before delivery, while the receipt
 * names the thread owner — this node, because it started the conversation. The outcome has to
 * carry the resolved addressee even when the spelling matched, or an initiator's send is
 * indistinguishable from a message this node addressed to itself; the alias's own spelling
 * rides beside it. And the entry has to actually reach the addressee: the grant push teaches
 * the peer the thread, its reconcile pulls the entry, and the peer's inbox holds it.
 */
async function phaseAddressee(): Promise<void> {
  const aliases: Record<string, string> = { dpdp: dpdpNode };
  const sender = serve(lazuliNode, base_url => rig(lazuliNode, base_url, {
    decide: async input => {
      const spelled = input.meta?.to;
      const to = typeof spelled === 'string' ? aliases[spelled] ?? spelled : undefined;
      if (to === undefined) throw new OnlyneError('decision_unavailable', 'The input named no target.');
      return { kind: 'send', to, response: 'async', execution: 'existing' };
    },
  }));
  const peer = serve(dpdpNode, base_url => rig(dpdpNode, base_url));

  const aliased = await sender.node.client.msg({ delivery_kind: '问候', msg: '你好 dpdp。', meta: { to: 'dpdp' } }) as SendOutcome;
  check('client.msg.resolved', aliased.admission === 'accepted' && aliased.requested === 'dpdp' && aliased.resolved === dpdpNode
    && aliased.receipt?.thread_ref.owner === lazuliNode
    ? '' : `an alias send reported ${JSON.stringify(aliased)}`);
  const named = await sender.node.client.msg({ delivery_kind: '问候', msg: '你好 dpdp。', meta: { to: dpdpNode } }) as SendOutcome;
  check('client.msg.named', named.admission === 'accepted' && named.resolved === dpdpNode && named.requested === undefined
    ? '' : `a node-id send reported ${JSON.stringify(named)}`);
  // The receipt alone cannot prove arrival: the addressee's own inbox must hold the entry.
  await peer.node.client.reconcile();
  const inbox = await peer.node.messages.inbox({ actor: dpdpNode }, {});
  const held = inbox.items.find(row => row.attention.message_ref.id === aliased.receipt?.message_ref.id);
  check('client.msg.peer-inbox', held !== undefined && held.attention.entry?.message.msg === '你好 dpdp。'
    ? '' : `the addressee held ${inbox.items.length} inbox rows without the sent message`);
}

/**
 * A send into a thread this node owns can reach its addressee only through a signed grant or
 * a signed reconcile, so a peer without a trust row can never see the entry. Such a send is
 * refused before anything is stored — `not_found`, no outbound record, nothing for the window
 * to re-drive — instead of an `accepted` that only the grant push could have earned.
 */
async function phaseInitiateTrust(): Promise<void> {
  // phaseAuth's keyed rig is local to that phase; this one needs only the signing half.
  const signing = (node_id: string, base_url: string, extra: Partial<Plugins> = {}): Rig => {
    const storage = new MemoryStorage();
    const shared: Shared = { injected: [], allow: true };
    const material = generateKeyMaterial();
    const plugins: Plugins = {
      storage,
      resolve: async (node: string): Promise<Target> => ({ node_id: node, base_url: urls[node] ?? base_url }),
      authenticate: async (): Promise<Auth | null> => null,
      decide: async () => { throw new OnlyneError('decision_unavailable', 'No plugin decision.'); },
      keys: async () => material,
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
    const options: NodeOptions = { node_id, base_url, plugins, now: () => clock.value, id: () => crypto.randomUUID(), wait_ms: 5_000 };
    return { node: new OnlyneNode(options), storage, shared, recreate: () => new OnlyneNode({ ...options }) };
  };
  const decide = { decide: async (input: { meta?: Record<string, Json> }) => {
    const to = input.meta?.to;
    if (typeof to !== 'string') throw new OnlyneError('decision_unavailable', 'The input named no target.');
    return { kind: 'send' as const, to, response: 'async' as const, execution: 'existing' as const };
  } };
  let discoveryDown = false;
  const start = serve(lazuliNode, base_url => signing(lazuliNode, base_url, {
    ...decide,
    resolve: async node => {
      if (discoveryDown && node === dpdpNode) throw new OnlyneError('not_found', 'No discovery record names this node.');
      return { node_id: node, base_url: urls[node] ?? base_url };
    },
  }));
  const peer = serve(dpdpNode, base_url => signing(dpdpNode, base_url));
  await start.node.trust.request({ actor: lazuliNode }, dpdpNode, (await peer.node.keys.describe())!.fingerprint);
  await peer.node.trust.request({ actor: dpdpNode }, lazuliNode, (await start.node.keys.describe())!.fingerprint);

  const first = await start.node.client.msg({ delivery_kind: '问候', msg: '你好 dpdp。', meta: { to: dpdpNode } }) as SendOutcome;
  check('initiate.trust.paired', first.admission === 'accepted' && first.resolved === dpdpNode
    ? '' : `a paired initiate reported ${JSON.stringify(first)}`);

  // The discovery catalog answers not_found, but the trust row carries the peer's address:
  // delivery falls back to it, and the address it proves is written back into the catalog,
  // so a peer that moved (or a catalog that expired) stays reachable through the pairing.
  discoveryDown = true;
  const third = await start.node.client.msg({ delivery_kind: '问候', msg: '换条路也要送到。', meta: { to: dpdpNode } }) as SendOutcome;
  check('address.trust-fallback', third.admission === 'accepted'
    ? '' : `a send with an empty discovery catalog reported ${JSON.stringify(third)}`);
  const written = await start.storage.transaction(async tx => tx.scan<{ node_id: string; source: string }>(key('discovery') + '/'));
  check('address.trust-fallback.written', written.some(row => row.value.node_id === dpdpNode && row.value.source === 'trust-row')
    ? '' : `the fallback left no discovery record: ${JSON.stringify(written.map(row => row.value))}`);

  await start.storage.transaction(async tx => tx.delete(key('trust', dpdpNode)));
  const second = await start.node.client.msg({ delivery_kind: '问候', msg: '还在吗?', meta: { to: dpdpNode } })
    .then(() => 'accepted', error => error instanceof OnlyneError ? error.error.code : String(error));
  check('initiate.trust.deleted', second === 'not_found'
    ? '' : `a send to an unpaired peer reported ${second}`);
  const stuck = await start.storage.transaction(async tx =>
    (await tx.scan<{ state: string }>(key('outbound') + '/'))
      .filter(row => row.value.state === 'pending' || row.value.state === 'unknown'));
  check('initiate.trust.deleted.no-retry', stuck.length === 0
    ? '' : `the refused send left ${stuck.length} records for the window to re-drive`);
}

/**
 * A pending exchange is confirmed by the acquisition policy on the side that owes the
 * approval, or by that node's holder. The failure must not prescribe a panel step, because
 * with policy all or segment the host submits the approval on the next attempt.
 */
async function phasePendingCopy(): Promise<void> {
  const row = (local_approved: boolean): TrustEntry => ({
    node_id: dpdpNode, public_key: '', fingerprint: '0123456789abcdef', state: 'pending',
    first_seen: new Date(clock.value).toISOString(), last_checked: new Date(clock.value).toISOString(),
    local_approved, remote_approved: !local_approved, base_url: 'http://127.0.0.1:9/onlyne/v3',
  });
  const pendingFailure = async (local_approved: boolean): Promise<string> => {
    const transport = new FetchTransport(
      async (node: string): Promise<Target> => ({ node_id: node, base_url: 'http://127.0.0.1:9/onlyne/v3' }),
      fetch,
      { trust: { entry: async () => row(local_approved), signRequest: async () => {}, verifyResponse: async () => {} } },
    );
    try {
      await transport.request(dpdpNode, 'POST', 'messages', {});
      return 'the request was signed';
    } catch (error) {
      return `${codeOf(error)}: ${text(error)}`;
}
  };
  for (const [branch, local_approved] of [['peer', true], ['local', false]] as const) {
    const copy = await pendingFailure(local_approved);
    check(`net.pending-copy.${branch}`, copy.startsWith('unavailable: ') && copy.includes('pending')
      && copy.includes('acquisition policy') && !copy.includes('Onlyne surface')
      ? '' : `the pending failure read: ${copy}`);
}
}

/**
 * A host that cannot hold an await or open a new execution context refuses the delivery at
 * admission. The plan is the only part the sender can change, so the refusal names it; a bare
 * "the runtime cannot fulfil this plan" leaves the sender guessing which half was refused.
 */
async function phasePlanRefusal(): Promise<void> {
  const sender = serve(lazuliNode, base_url => rig(lazuliNode, base_url));
  serve(dpdpNode, base_url => rig(dpdpNode, base_url, {
    runtime: {
      // This host can deliver into an existing context and cannot open one.
      supports: async plan => plan.execution !== 'new',
      notify: event => notified.push({ node: dpdpNode, event }),
      inject: async (entry: Entry, context?: string) => {
        void entry;
        return { context: context ?? dpdpNode, consumed: true };
      },
      todo: async () => {},
    },
  }));
  // A thread the receiver owns: the receiver is the node that admits, so its plan check runs.
  const outcome = await sender.node.client.send(dpdpNode, {
    id: 'plan-01', sender: lazuliNode, delivery_kind: '新上下文', msg: '这条要求接收方开一个新上下文。',
    meta: { thread: { owner: dpdpNode, id: 'plan-thread' } },
  }, { response: 'async', execution: 'new' });
  check('plan.refused', outcome.admission === 'refused' && outcome.error?.code === 'unsupported_delivery'
    ? '' : `the refused plan reported ${outcome.admission}/${outcome.error?.code ?? 'none'}: ${JSON.stringify(outcome)}`);
  // Both halves of the plan are in the text, because the sender chooses both.
  check('plan.named', outcome.error?.message.includes('async response') === true
    && outcome.error.message.includes('new execution') ? '' : `the refusal read: ${outcome.error?.message ?? 'none'}`);
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
  await phaseJoined();
  await phaseRecoverOutbound();
  await phaseExpiry();
  await phaseResources();
  await phaseHandoverRecovery();
  await phaseSettlement();
  await phaseAuth();
  await phaseAddressee();
  await phaseInitiateTrust();
  await phasePendingCopy();
  await phasePlanRefusal();
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
