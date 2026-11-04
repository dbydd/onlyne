import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { OnlyneError, type Entry } from '@onlyne/lib';
import {
  createGrokBotNode, expandEnv, grokBotChatFromMessage, grokBotThreadId, GrokBotPullClient,
  loadConfig, parseGrokBotThreadId, webhookHeaderValue, webhookPayload,
  type FetchLike, type GrokBotConfig,
} from '../src/index.ts';

let failures = 0;
const ok = (label: string, condition: boolean): void => {
  if (condition) console.log(`ok - ${label}`);
  else { failures++; console.error(`not ok - ${label}`); }
};

const settle = (ms = 30): Promise<void> => new Promise(resolve => setTimeout(resolve, ms));

const waitFor = async (predicate: () => boolean | Promise<boolean>, timeoutMs = 2_000): Promise<boolean> => {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await predicate()) return true;
    await settle(15);
  }
  return false;
};

const GROK = 'urn:onlyne:gateway:grok-bot';
const SENDER = 'urn:onlyne:agent:alice';
const BOB = 'urn:onlyne:agent:bob';
const KEY = 'test-sender-key-do-not-commit';
const aliceDm = grokBotThreadId({ kind: 'dm', chat_id: SENDER });
const bobDm = grokBotThreadId({ kind: 'dm', chat_id: BOB });
const rec = (value: unknown): Record<string, unknown> | undefined =>
  value !== null && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : undefined;

interface Posted {
  url: string;
  headers: Record<string, string>;
  body: Record<string, unknown>;
}

const dir = await mkdtemp(join(tmpdir(), 'grok-bot-'));

// --- config -----------------------------------------------------------------

const configPath = join(dir, 'grok-bot.json');
await writeFile(configPath, JSON.stringify({
  node: { id: GROK },
  server: { bind: '127.0.0.1', port: 0 },
  storage: { path: join(dir, 'store.json') },
  webhook: {
    url: '$GROK_BOT_WEBHOOK_URL',
    key: '${GROK_BOT_WEBHOOK_KEY}',
  },
}));

try {
  await loadConfig(configPath, {});
  ok('loadConfig refuses a missing webhook variable', false);
} catch (error) {
  ok('loadConfig refuses a missing webhook variable',
    error instanceof OnlyneError && error.error.code === 'invalid_request'
    && error.error.message.includes('GROK_BOT_WEBHOOK_URL'));
}

const loaded = await loadConfig(configPath, {
  GROK_BOT_WEBHOOK_URL: 'http://127.0.0.1:9/wake',
  GROK_BOT_WEBHOOK_KEY: KEY,
});
ok('loadConfig expands $VAR and ${VAR} from the environment',
  loaded.webhook.url === 'http://127.0.0.1:9/wake' && loaded.webhook.key === KEY
  && loaded.webhook.header === 'Authorization' && loaded.webhook.scheme === 'Bearer');

const overridden = await loadConfig(configPath, {
  GROK_BOT_WEBHOOK_URL: 'http://127.0.0.1:9/wake',
  GROK_BOT_WEBHOOK_KEY: KEY,
  ONLYNE_GROK_WEBHOOK_URL: 'https://hooks.example.test/wake',
  ONLYNE_GROK_WEBHOOK_KEY: 'override-key',
  ONLYNE_GROK_WEBHOOK_HEADER: 'X-Grok-Token',
  ONLYNE_GROK_WEBHOOK_SCHEME: '',
  ONLYNE_NODE_ID: 'urn:onlyne:gateway:grok-bot-test',
  ONLYNE_PORT: '4010',
});
ok('ONLYNE_GROK_* and ONLYNE_NODE_ID / PORT override the file',
  overridden.webhook.url === 'https://hooks.example.test/wake'
  && overridden.webhook.key === 'override-key'
  && overridden.webhook.header === 'X-Grok-Token'
  && overridden.webhook.scheme === ''
  && overridden.node.id === 'urn:onlyne:gateway:grok-bot-test'
  && overridden.server.port === 4010);

ok('$$ expands to a literal dollar', expandEnv('cost-$$5', {}, 'x') === 'cost-$5');

ok('DM thread id is grokbot:dm:<agent_id>',
  grokBotThreadId({ kind: 'dm', chat_id: SENDER }) === `grokbot:dm:${SENDER}`
  && parseGrokBotThreadId(`grokbot:dm:${SENDER}`)?.kind === 'dm'
  && parseGrokBotThreadId(`grokbot:dm:${SENDER}`)?.chat_id === SENDER);
ok('group thread id is grokbot:group:<group_id>',
  grokBotThreadId({ kind: 'group', chat_id: 'room-9' }) === 'grokbot:group:room-9'
  && parseGrokBotThreadId('grokbot:group:room-9')?.kind === 'group'
  && parseGrokBotThreadId('grokbot:group:room-9')?.chat_id === 'room-9');
ok('a random UUID is not a grok-bot thread id', parseGrokBotThreadId(crypto.randomUUID()) === undefined);
ok('meta.data.grokbot selects a group chat',
  grokBotChatFromMessage({
    id: 'm', sender: SENDER, delivery_kind: 'note', msg: 'hi',
    meta: { data: { grokbot: { chat_kind: 'group', chat_id: 'room-9' } } },
  }, SENDER).kind === 'group'
  && grokBotChatFromMessage({
    id: 'm', sender: SENDER, delivery_kind: 'note', msg: 'hi',
    meta: { data: { grokbot: { chat_kind: 'group', chat_id: 'room-9' } } },
  }, SENDER).chat_id === 'room-9');
ok('a message without grokbot fields is a DM with the sender',
  grokBotChatFromMessage({ id: 'm', sender: SENDER, delivery_kind: 'note', msg: 'hi' }, SENDER).chat_id === SENDER);
ok('the default Authorization value is Bearer plus the key',
  webhookHeaderValue({ key: KEY, scheme: 'Bearer' }) === `Bearer ${KEY}`);
ok('an empty scheme sends the raw key', webhookHeaderValue({ key: KEY, scheme: '' }) === KEY);

try {
  await loadConfig(configPath, { GROK_BOT_WEBHOOK_URL: 'not-a-url', GROK_BOT_WEBHOOK_KEY: KEY });
  ok('a non-URL webhook.url is refused', false);
} catch (error) {
  ok('a non-URL webhook.url is refused', error instanceof OnlyneError);
}

const fileText = await Bun.file(configPath).text();
ok('the committed-shape config file does not contain the live key',
  !fileText.includes(KEY) && (fileText.includes('$GROK_BOT_WEBHOOK_KEY') || fileText.includes('${GROK_BOT_WEBHOOK_KEY}')));

const pinPath = join(dir, 'grok-bot-pin.json');
await writeFile(pinPath, JSON.stringify({
  node: { id: GROK },
  server: { bind: '127.0.0.1', port: 0 },
  storage: { path: join(dir, 'store.json') },
  webhook: { url: '$GROK_BOT_WEBHOOK_URL', key: '${GROK_BOT_WEBHOOK_KEY}' },
  trust_seeds: [{ node_id: SENDER, fingerprint: '0123456789abcdef' }],
}));
const pinned = await loadConfig(pinPath, {
  GROK_BOT_WEBHOOK_URL: 'http://127.0.0.1:9/wake',
  GROK_BOT_WEBHOOK_KEY: KEY,
});
ok('loadConfig reads pinned trust_seeds',
  pinned.trust_seeds?.length === 1
  && pinned.trust_seeds[0]?.node_id === SENDER
  && pinned.trust_seeds[0]?.fingerprint === '0123456789abcdef');

const envPinned = await loadConfig(configPath, {
  GROK_BOT_WEBHOOK_URL: 'http://127.0.0.1:9/wake',
  GROK_BOT_WEBHOOK_KEY: KEY,
  ONLYNE_GROK_TRUST_SEEDS: JSON.stringify([{ node_id: BOB, fingerprint: 'AABBCCDDEEFF0011' }]),
});
ok('ONLYNE_GROK_TRUST_SEEDS overrides pins and lowercases the fingerprint',
  envPinned.trust_seeds?.length === 1
  && envPinned.trust_seeds[0]?.node_id === BOB
  && envPinned.trust_seeds[0]?.fingerprint === 'aabbccddeeff0011');

try {
  await loadConfig(pinPath, {
    GROK_BOT_WEBHOOK_URL: 'http://127.0.0.1:9/wake',
    GROK_BOT_WEBHOOK_KEY: KEY,
    ONLYNE_GROK_TRUST_SEEDS: JSON.stringify([{ node_id: SENDER, fingerprint: '*' }]),
  });
  ok('a wildcard fingerprint is refused', false);
} catch (error) {
  ok('a wildcard fingerprint is refused', error instanceof OnlyneError);
}

try {
  await loadConfig(configPath, {
    GROK_BOT_WEBHOOK_URL: 'http://127.0.0.1:9/wake',
    GROK_BOT_WEBHOOK_KEY: KEY,
    ONLYNE_GROK_TRUST_SEEDS: JSON.stringify([
      { node_id: SENDER, fingerprint: '0123456789abcdef' },
      { node_id: SENDER, fingerprint: '0123456789abcde0' },
    ]),
  });
  ok('a duplicate pin node_id is refused', false);
} catch (error) {
  ok('a duplicate pin node_id is refused', error instanceof OnlyneError);
}

// --- inbound POST -----------------------------------------------------------

const posts: Posted[] = [];
let webhookStatus: number | ((call: number) => number) = 204;
let webhookDelayMs = 0;
let webhookCalls = 0;
const mockFetch: FetchLike = async (input, init) => {
  webhookCalls += 1;
  const call = webhookCalls;
  if (webhookDelayMs > 0) await settle(webhookDelayMs);
  const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
  const headers: Record<string, string> = {};
  const raw = init?.headers;
  if (raw !== undefined && !(raw instanceof Headers) && !Array.isArray(raw)) {
    for (const [name, value] of Object.entries(raw)) headers[name.toLowerCase()] = String(value);
  }
  const parsed = rec(JSON.parse(String(init?.body ?? '{}')));
  posts.push({ url, headers, body: parsed ?? {} });
  const status = typeof webhookStatus === 'function' ? webhookStatus(call) : webhookStatus;
  return new Response(null, { status });
};

const config = (over: Partial<GrokBotConfig['webhook']> = {}): GrokBotConfig => ({
  node: { id: GROK },
  server: { bind: '127.0.0.1', port: 0 },
  storage_path: join(dir, `store-${crypto.randomUUID()}.json`),
  webhook: {
    url: 'http://127.0.0.1:9/wake',
    key: KEY,
    header: 'Authorization',
    scheme: 'Bearer',
    timeout_ms: 200,
    max_attempts: 4,
    retry_base_ms: 1,
    ...over,
  },
  reply_path: '/grok/reply',
  events_path: '/grok/events',
  ack_path: '/grok/events/ack',
});

const logs: Array<{ message: string; fields?: Record<string, unknown> }> = [];
const log = (message: string, fields?: Record<string, unknown>): void => { logs.push({ message, fields }); };

const grok = await createGrokBotNode(config(), { storage: 'memory', fetch: mockFetch, log });
const mut = () => ({
  idempotency_key: crypto.randomUUID(),
  retry_until: new Date(Date.now() + 60_000).toISOString(),
});

const admitOn = async (
  target: Awaited<ReturnType<typeof createGrokBotNode>>,
  sender: string,
  id: string,
  msg: string,
  meta?: Record<string, unknown>,
) => target.node.messages.admit({ actor: sender }, {
  ...mut(),
  message: {
    id, sender, delivery_kind: 'note', msg,
    ...(meta === undefined ? {} : { meta: meta as never }),
  },
  target_endpoint: GROK,
  plan: { response: 'none', execution: 'existing' },
});
const admit = (id: string, msg: string, meta?: Record<string, unknown>) =>
  admitOn(grok, SENDER, id, msg, meta);

const firstReceipt = await admit('msg-01', 'hello grok');
ok('the first message on a new grokbot:dm thread reports thread_created',
  firstReceipt.thread_created === true && firstReceipt.seq === '1');
ok('an inbound admission POSTs the webhook', await waitFor(() => posts.length >= 1) && posts.length >= 1);

const first = posts[0];
const firstBody = first?.body ?? {};
ok('the POST targets the configured URL', first?.url === 'http://127.0.0.1:9/wake');
ok('the sender key is an Authorization Bearer header',
  first?.headers.authorization === `Bearer ${KEY}`);
ok('the JSON body names sender, message id, timestamp, content and thread',
  firstBody.sender === SENDER
  && firstBody.message_id === 'msg-01'
  && firstBody.msg === 'hello grok'
  && typeof firstBody.timestamp === 'string'
  && rec(firstBody.thread)?.owner === GROK
  && rec(firstBody.thread)?.id === aliceDm
  && rec(firstBody.message)?.id === 'msg-01');
const firstMeta = rec(firstBody.meta);
const firstGrokbot = rec(firstMeta?.grokbot);
ok('the webhook meta carries grokbot.chat_kind and grokbot.chat_id',
  firstGrokbot?.chat_kind === 'dm' && firstGrokbot?.chat_id === SENDER);

const logged = JSON.stringify(logs);
ok('logs never include the sender key', !logged.includes(KEY));

await admit('msg-01', 'hello grok');
const afterDup = posts.length;
await settle(80);
ok('a duplicate admission does not POST a second webhook', posts.length === afterDup);

const secondReceipt = await admit('msg-02-created', 'still alice');
ok('a later message on the same grokbot:dm thread reports thread_created false',
  secondReceipt.thread_created === false && secondReceipt.seq !== '1');
ok('the second message also POSTs the webhook',
  await waitFor(() => posts.some(post => post.body.message_id === 'msg-02-created')));

// --- retries ----------------------------------------------------------------

webhookStatus = (call: number) => (call === 1 ? 503 : 204);
posts.length = 0;
webhookCalls = 0;
const failing = await createGrokBotNode(config(), { storage: 'memory', fetch: mockFetch, log });
await failing.node.messages.admit({ actor: SENDER }, {
  ...mut(),
  message: { id: 'msg-503', sender: SENDER, delivery_kind: 'note', msg: 'retry me' },
  target_endpoint: GROK,
  plan: { response: 'none', execution: 'existing' },
});
ok('a 503 retries and then delivers', await waitFor(async () => {
  await failing.maintain();
  return posts.length >= 2;
}, 3_000) && posts.length >= 2 && posts[0]?.body.message_id === 'msg-503');
await failing.stop();

webhookStatus = 401;
posts.length = 0;
webhookCalls = 0;
const unauthorized = await createGrokBotNode(config({ max_attempts: 3 }), { storage: 'memory', fetch: mockFetch, log });
await unauthorized.node.messages.admit({ actor: SENDER }, {
  ...mut(),
  message: { id: 'msg-401', sender: SENDER, delivery_kind: 'note', msg: 'no retry' },
  target_endpoint: GROK,
  plan: { response: 'none', execution: 'existing' },
});
await settle(80);
ok('a 401 is terminal and does not retry', posts.length === 1);
const unauthStatus = await unauthorized.status();
ok('a terminal failure stays visible on /grok/status', unauthStatus.queue.failed >= 1);
await unauthorized.stop();

webhookStatus = 204;
posts.length = 0;
webhookCalls = 0;
let concurrent = 0;
let maxConcurrent = 0;
const concurrentFetch: FetchLike = async (input, init) => {
  concurrent += 1;
  maxConcurrent = Math.max(maxConcurrent, concurrent);
  try {
    return await mockFetch(input, init);
  } finally {
    concurrent -= 1;
  }
};
webhookDelayMs = 50;
const slow = await createGrokBotNode(config(), { storage: 'memory', fetch: concurrentFetch, log });
await Promise.all([
  slow.node.messages.admit({ actor: SENDER }, {
    ...mut(),
    message: { id: 'msg-a', sender: SENDER, delivery_kind: 'note', msg: 'one' },
    target_endpoint: GROK,
    plan: { response: 'none', execution: 'existing' },
  }),
  slow.node.messages.admit({ actor: SENDER }, {
    ...mut(),
    message: { id: 'msg-b', sender: SENDER, delivery_kind: 'note', msg: 'two' },
    target_endpoint: GROK,
    plan: { response: 'none', execution: 'existing' },
  }),
]);
ok('two inbound messages both reach the webhook', await waitFor(() => posts.length >= 2, 3_000));
ok('a slow webhook does not block the other message', posts.length >= 2 && maxConcurrent >= 2);
webhookDelayMs = 0;
await slow.stop();

posts.length = 0;
webhookCalls = 0;
const hanging: FetchLike = async (_input, init) => {
  const signal = init?.signal;
  if (signal?.aborted) {
    throw Object.assign(new Error('timeout'), { name: 'TimeoutError' });
  }
  await new Promise<void>((resolve, reject) => {
    const timer = setTimeout(resolve, 400);
    signal?.addEventListener('abort', () => {
      clearTimeout(timer);
      reject(Object.assign(new Error('timeout'), { name: 'TimeoutError' }));
    });
  });
  return new Response(null, { status: 204 });
};
const timed = await createGrokBotNode(config({ timeout_ms: 40, max_attempts: 2, retry_base_ms: 1 }), {
  storage: 'memory', fetch: hanging, log,
});
await timed.node.messages.admit({ actor: SENDER }, {
  ...mut(),
  message: { id: 'msg-to', sender: SENDER, delivery_kind: 'note', msg: 'slow' },
  target_endpoint: GROK,
  plan: { response: 'none', execution: 'existing' },
});
ok('a timeout is retried then marked failed once attempts run out', await waitFor(async () => {
  await timed.maintain();
  const report = await timed.status();
  return report.queue.failed >= 1 || report.queue.attempts >= 2;
}, 3_000));
await timed.stop();

// --- custom header ----------------------------------------------------------

posts.length = 0;
webhookCalls = 0;
webhookStatus = 204;
const custom = await createGrokBotNode(config({ header: 'X-Grok-Token', scheme: '' }), {
  storage: 'memory', fetch: mockFetch, log,
});
await custom.node.messages.admit({ actor: SENDER }, {
  ...mut(),
  message: { id: 'msg-hdr', sender: SENDER, delivery_kind: 'note', msg: 'header' },
  target_endpoint: GROK,
  plan: { response: 'none', execution: 'existing' },
});
ok('a custom header name carries the raw key',
  await waitFor(() => posts.length >= 1)
  && posts[0]?.headers['x-grok-token'] === KEY
  && posts[0]?.headers.authorization === undefined);
await custom.stop();

// --- payload helper ---------------------------------------------------------

const sample = {
  thread_ref: { owner: GROK, id: 'thr-1' },
  seq: '3',
  message: { id: 'm', sender: SENDER, delivery_kind: 'note', msg: 'hi', meta: { mentions: [GROK] } },
  authenticated_author: SENDER,
  reply_authority: SENDER,
  at: '2026-11-03T00:00:00.000Z',
} as Entry;
const payload = rec(webhookPayload(GROK, sample));
ok('webhookPayload includes sender, message id, timestamp and metadata',
  payload?.sender === SENDER && payload?.message_id === 'm'
  && payload?.timestamp === '2026-11-03T00:00:00.000Z'
  && rec(payload?.meta)?.mentions !== undefined);
ok('webhookPayload falls back to a DM keyed by the sender for a legacy thread id',
  rec(rec(payload?.meta)?.grokbot)?.chat_kind === 'dm'
  && rec(rec(payload?.meta)?.grokbot)?.chat_id === SENDER);

const stableSample = {
  ...sample,
  thread_ref: { owner: GROK, id: 'grokbot:group:room-9' },
} as Entry;
const stableMeta = rec(rec(webhookPayload(GROK, stableSample))?.meta);
const stableGrokbot = rec(stableMeta?.grokbot);
ok('webhookPayload reads grokbot.chat_kind/chat_id from a stable thread id',
  stableGrokbot?.chat_kind === 'group' && stableGrokbot?.chat_id === 'room-9');

// --- thread isolation -------------------------------------------------------

posts.length = 0;
webhookCalls = 0;
webhookStatus = 204;
const isolated = await createGrokBotNode(config(), { storage: 'memory', fetch: mockFetch, log });
await admitOn(isolated, SENDER, 'iso-a1', 'alice first');
await admitOn(isolated, SENDER, 'iso-a2', 'alice second');
await admitOn(isolated, BOB, 'iso-b1', 'bob first');
await admitOn(isolated, SENDER, 'iso-g1', 'group one', {
  data: { grokbot: { chat_kind: 'group', chat_id: 'room-1' } },
});
await admitOn(isolated, BOB, 'iso-g2', 'group two', {
  data: { grokbot: { chat_kind: 'group', chat_id: 'room-2' } },
});
ok('two messages from the same sender share grokbot:dm:<sender>', await waitFor(async () => {
  const listed = await isolated.node.messages.list(isolated.node.auth, { owner: GROK, id: aliceDm }, {
    attention: 'thread', page_size: 50,
  });
  return listed.items.some(entry => entry.message.id === 'iso-a1')
    && listed.items.some(entry => entry.message.id === 'iso-a2');
}));
const aliceThread = await isolated.node.messages.list(isolated.node.auth, { owner: GROK, id: aliceDm }, {
  attention: 'thread', page_size: 50,
});
const bobThread = await isolated.node.messages.list(isolated.node.auth, { owner: GROK, id: bobDm }, {
  attention: 'thread', page_size: 50,
});
ok('a second sender lands on its own DM thread',
  bobThread.items.some(entry => entry.message.id === 'iso-b1')
  && aliceThread.items.every(entry => entry.message.sender !== BOB)
  && bobThread.items.every(entry => entry.message.sender !== SENDER));
const groupOne = await isolated.node.messages.list(isolated.node.auth, { owner: GROK, id: 'grokbot:group:room-1' }, {
  attention: 'thread', page_size: 50,
});
const groupTwo = await isolated.node.messages.list(isolated.node.auth, { owner: GROK, id: 'grokbot:group:room-2' }, {
  attention: 'thread', page_size: 50,
});
ok('each Grok Bot group chat has its own thread',
  groupOne.items.some(entry => entry.message.id === 'iso-g1')
  && groupTwo.items.some(entry => entry.message.id === 'iso-g2')
  && groupOne.items.every(entry => entry.message.id !== 'iso-g2')
  && aliceThread.items.every(entry => entry.message.id !== 'iso-g1'));
ok('group webhook meta names grokbot.chat_kind=group',
  posts.some(post => rec(rec(post.body.meta)?.grokbot)?.chat_kind === 'group'
    && rec(rec(post.body.meta)?.grokbot)?.chat_id === 'room-1'));

// --- outbound reply ---------------------------------------------------------

const replyNode = await createGrokBotNode(config(), { storage: 'memory', fetch: mockFetch, log });
const denied = await replyNode.fetch(new Request('http://127.0.0.1/grok/reply', {
  method: 'POST',
  headers: { 'content-type': 'application/json' },
  body: JSON.stringify({ to: SENDER, msg: 'nope' }),
}));
ok('the reply path refuses a missing sender key', denied.status === 401);

const badKey = await replyNode.fetch(new Request('http://127.0.0.1/grok/reply', {
  method: 'POST',
  headers: { 'content-type': 'application/json', Authorization: 'Bearer wrong' },
  body: JSON.stringify({ to: SENDER, msg: 'nope' }),
}));
ok('the reply path refuses a wrong sender key', badKey.status === 401);

posts.length = 0;
webhookCalls = 0;
await admitOn(replyNode, SENDER, 'reply-src', 'please acknowledge');
ok('the inbound that a reply will continue reached the webhook', await waitFor(() =>
  posts.some(post => post.body.message_id === 'reply-src')));
const inbound = posts.find(post => post.body.message_id === 'reply-src')?.body ?? {};
const inboundThread = rec(inbound.thread);

const omitted = await replyNode.fetch(new Request('http://127.0.0.1/grok/reply', {
  method: 'POST',
  headers: { 'content-type': 'application/json', Authorization: `Bearer ${KEY}` },
  body: JSON.stringify({ to: GROK, msg: 'orphan reply', delivery_kind: 'reply' }),
}));
const omittedBody = rec(JSON.parse(await omitted.text()));
ok('a reply that omits thread and in_reply_to is refused',
  omitted.status !== 201
  && omittedBody?.admission !== 'accepted'
  && (rec(omittedBody?.error)?.code === 'not_found' || rec(omittedBody?.error)?.code !== undefined));

const sent = await replyNode.fetch(new Request('http://127.0.0.1/grok/reply', {
  method: 'POST',
  headers: { 'content-type': 'application/json', Authorization: `Bearer ${KEY}` },
  body: JSON.stringify({
    to: GROK,
    id: 'reply-ack',
    msg: 'ack from grok',
    delivery_kind: 'reply',
    thread: inboundThread,
    in_reply_to: { sender: SENDER, id: 'reply-src' },
  }),
}));
const sentBody = rec(JSON.parse(await sent.text()));
const sentReceipt = rec(sentBody?.receipt);
ok('an authenticated reply is sent through this node',
  sent.status === 201 && sentBody?.admission === 'accepted');
ok('a reply that brings back the inbound thread lands on that thread_ref',
  rec(sentReceipt?.thread_ref)?.owner === GROK
  && rec(sentReceipt?.thread_ref)?.id === aliceDm
  && rec(sentReceipt?.thread_ref)?.id === inboundThread?.id);
ok('a /grok/reply is not posted back as a webhook wake',
  posts.every(post => post.body.message_id !== 'reply-ack' && post.body.sender !== GROK));
ok('a /grok/reply is not offered on the pull inbox',
  !(await replyNode.pull.pull({ wait_ms: 0 })).some(item => rec(item.payload)?.sender === GROK
    || rec(item.payload)?.message_id === 'reply-ack'));

const continued = await replyNode.node.messages.list(replyNode.node.auth, { owner: GROK, id: aliceDm }, {
  attention: 'thread', page_size: 50,
});
ok('the continued thread holds both the inbound note and the reply',
  continued.items.some(entry => entry.message.id === 'reply-src')
  && continued.items.some(entry => entry.message.id === 'reply-ack'
    && entry.message.meta?.in_reply_to?.sender === SENDER
    && entry.message.meta?.in_reply_to?.id === 'reply-src'));

const recovered = await replyNode.fetch(new Request('http://127.0.0.1/grok/reply', {
  method: 'POST',
  headers: { 'content-type': 'application/json', Authorization: `Bearer ${KEY}` },
  body: JSON.stringify({
    to: GROK,
    id: 'reply-recovered',
    msg: 'recovered by in_reply_to',
    delivery_kind: 'reply',
    in_reply_to: { sender: SENDER, id: 'reply-src' },
  }),
}));
const recoveredBody = rec(JSON.parse(await recovered.text()));
ok('omitting thread but sending in_reply_to recovers the original landing',
  recovered.status === 201
  && rec(rec(recoveredBody?.receipt)?.thread_ref)?.id === aliceDm);

const statusRes = await replyNode.fetch(new Request('http://127.0.0.1/grok/status'));
const statusBody = rec(JSON.parse(await statusRes.text()));
const webhook = rec(statusBody?.webhook);
ok('GET /grok/status redacts the sender key',
  statusRes.status === 200 && webhook?.key === 'set' && JSON.stringify(statusBody).includes(KEY) === false);

// --- outbound-only pull / ack ------------------------------------------------

posts.length = 0;
webhookCalls = 0;
webhookStatus = 204;
let clock = Date.now();
const pullNode = await createGrokBotNode(config(), { storage: 'memory', fetch: mockFetch, log, now: () => clock });
const eventsUrl = 'http://127.0.0.1/grok/events';
const eventsHeaders = { Authorization: `Bearer ${KEY}` };
const deniedEvents = await pullNode.fetch(new Request(eventsUrl));
ok('GET /grok/events refuses a missing sender key', deniedEvents.status === 401);

await admitOn(pullNode, SENDER, 'pull-offline', 'queued while the device is gone');
ok('an inbound while no puller is connected still POSTs the webhook',
  await waitFor(() => posts.some(post => post.body.message_id === 'pull-offline')));

const pulled = rec(JSON.parse(await (await pullNode.fetch(new Request(`${eventsUrl}?wait_ms=0&lease_ms=50`, {
  headers: eventsHeaders,
}))).text()));
const pulledItems = Array.isArray(pulled?.items) ? pulled.items : [];
const firstPull = rec(pulledItems[0]);
const firstPayload = rec(firstPull?.payload);
ok('a reconnecting device pulls the offline message',
  pulledItems.length >= 1 && firstPayload?.message_id === 'pull-offline'
  && rec(rec(firstPayload?.meta)?.grokbot)?.chat_kind === 'dm'
  && rec(firstPayload?.thread)?.id === aliceDm);

const leasedEmpty = rec(JSON.parse(await (await pullNode.fetch(new Request(`${eventsUrl}?wait_ms=0`, {
  headers: eventsHeaders,
}))).text()));
ok('a leased row is not offered again until the lease expires',
  Array.isArray(leasedEmpty?.items) && leasedEmpty.items.length === 0);

clock += 51;
const redelivered = rec(JSON.parse(await (await pullNode.fetch(new Request(`${eventsUrl}?wait_ms=0&lease_ms=5000`, {
  headers: eventsHeaders,
}))).text()));
const redeliveredItems = Array.isArray(redelivered?.items) ? redelivered.items : [];
ok('an unacked lease expires and the same delivery id is offered again',
  redeliveredItems.some(item => rec(item)?.id === firstPull?.id));

const acked = rec(JSON.parse(await (await pullNode.fetch(new Request('http://127.0.0.1/grok/events/ack', {
  method: 'POST',
  headers: { ...eventsHeaders, 'content-type': 'application/json' },
  body: JSON.stringify({ ids: [firstPull?.id] }),
}))).text()));
ok('ack records the pulled delivery',
  Array.isArray(acked?.acked) && acked.acked.includes(String(firstPull?.id)));

clock += 10_000;
const afterAck = rec(JSON.parse(await (await pullNode.fetch(new Request(`${eventsUrl}?wait_ms=0`, {
  headers: eventsHeaders,
}))).text()));
ok('an acked delivery is not redelivered',
  Array.isArray(afterAck?.items) && afterAck.items.length === 0);

const waiting = pullNode.fetch(new Request(`${eventsUrl}?wait_ms=1500&lease_ms=5000`, {
  headers: eventsHeaders,
}));
await settle(40);
await admitOn(pullNode, SENDER, 'pull-live', 'arrived during long-poll');
const liveBody = rec(JSON.parse(await (await waiting).text()));
const liveItems = Array.isArray(liveBody?.items) ? liveBody.items : [];
ok('a long-poll returns when a new inbound is queued',
  liveItems.some(item => rec(rec(item)?.payload)?.message_id === 'pull-live'));

const client = new GrokBotPullClient({
  origin: 'http://127.0.0.1',
  webhook: pullNode.config.webhook,
  fetch: (input, init) => pullNode.fetch(new Request(input, init)),
  wait_ms: 0,
  lease_ms: 5_000,
});
await admitOn(pullNode, SENDER, 'pull-client', 'via helper');
const clientItems = await client.pull({ wait_ms: 1_000 });
const clientHit = clientItems.find(item => rec(item.payload)?.message_id === 'pull-client');
ok('GrokBotPullClient pulls the webhook-equivalent payload', clientHit !== undefined);
if (clientHit !== undefined) {
  const ackResult = await client.ack([clientHit.id]);
  ok('GrokBotPullClient acks the delivery', ackResult.acked.includes(clientHit.id));
  const inboundThread = rec(clientHit.payload)?.thread;
  const replied = await client.reply({
    to: GROK,
    id: 'pull-client-ack',
    msg: 'pulled then replied',
    thread: rec(inboundThread) as { owner: string; id: string } | undefined,
    in_reply_to: { sender: SENDER, id: 'pull-client' },
  });
  ok('GrokBotPullClient posts /grok/reply on the inbound thread',
    replied.status === 201 && rec(replied.body)?.admission === 'accepted');
}

const pullStatus = await pullNode.status();
ok('status reports the pull inbox separately from the webhook queue',
  pullStatus.pull.acked >= 1 && pullStatus.events_path === '/grok/events');

ok('GrokBotPullClient reply does not re-queue a self-authored wake',
  !(await pullNode.pull.pull({ wait_ms: 0 })).some(item => rec(item.payload)?.sender === GROK
    || rec(item.payload)?.message_id === 'pull-client-ack'));

webhookStatus = 503;
const persistPath = join(dir, `persist-${crypto.randomUUID()}.json`);
const persistA = await createGrokBotNode({ ...config(), storage_path: persistPath }, { fetch: mockFetch, log });
await admitOn(persistA, SENDER, 'persist-01', 'keep me across restart');
ok('owed webhook and pull rows exist before restart', await waitFor(async () => {
  const pull = await persistA.pull.snapshot();
  const web = await persistA.dispatcher.snapshot();
  return pull.owed + pull.leased >= 1 && web.queue.owed + web.queue.failed >= 1;
}));
await persistA.stop();
const persistB = await createGrokBotNode({ ...config(), storage_path: persistPath }, { fetch: mockFetch, log });
const pullB = await persistB.pull.snapshot();
const webB = await persistB.dispatcher.snapshot();
ok('owed pull rows survive a process restart', pullB.owed + pullB.leased >= 1);
ok('owed webhook rows survive a process restart', webB.queue.owed + webB.queue.failed >= 1);
const persistPulled = await persistB.pull.pull({ wait_ms: 0 });
ok('the persisted pull row is the same inbound',
  persistPulled.some(item => rec(item.payload)?.message_id === 'persist-01'));
webhookStatus = 204;
ok('webhook retry after restart delivers the persisted row', await waitFor(async () => {
  await persistB.maintain();
  return (await persistB.dispatcher.snapshot()).queue.delivered >= 1;
}));
await persistB.stop();

await pullNode.stop();
await isolated.stop();
await replyNode.stop();
await grok.stop();
await rm(dir, { recursive: true, force: true });

if (failures > 0) {
  console.error(`grok-bot verify: ${failures} check(s) failed`);
  process.exit(1);
}
console.log('grok-bot verify: all checks passed');
