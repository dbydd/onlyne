import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { OnlyneError, type Entry } from '@onlyne/lib';
import {
  createGrokBotNode, expandEnv, loadConfig, webhookHeaderValue, webhookPayload,
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
const KEY = 'test-sender-key-do-not-commit';
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
});

const logs: Array<{ message: string; fields?: Record<string, unknown> }> = [];
const log = (message: string, fields?: Record<string, unknown>): void => { logs.push({ message, fields }); };

const grok = await createGrokBotNode(config(), { storage: 'memory', fetch: mockFetch, log });
const mut = () => ({
  idempotency_key: crypto.randomUUID(),
  retry_until: new Date(Date.now() + 60_000).toISOString(),
});

const admit = async (id: string, msg: string): Promise<void> => {
  await grok.node.messages.admit({ actor: SENDER }, {
    ...mut(),
    message: { id, sender: SENDER, delivery_kind: 'note', msg },
    target_endpoint: GROK,
    plan: { response: 'none', execution: 'existing' },
  });
};

await admit('msg-01', 'hello grok');
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
  && rec(firstBody.message)?.id === 'msg-01');

const logged = JSON.stringify(logs);
ok('logs never include the sender key', !logged.includes(KEY));

await admit('msg-01', 'hello grok');
const afterDup = posts.length;
await settle(80);
ok('a duplicate admission does not POST a second webhook', posts.length === afterDup);

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

const sent = await replyNode.fetch(new Request('http://127.0.0.1/grok/reply', {
  method: 'POST',
  headers: { 'content-type': 'application/json', Authorization: `Bearer ${KEY}` },
  body: JSON.stringify({ to: GROK, msg: 'ack from grok', delivery_kind: 'reply' }),
}));
const sentBody = rec(JSON.parse(await sent.text()));
ok('an authenticated reply is sent through this node',
  sent.status === 201 && sentBody?.admission === 'accepted');

const statusRes = await replyNode.fetch(new Request('http://127.0.0.1/grok/status'));
const statusBody = rec(JSON.parse(await statusRes.text()));
const webhook = rec(statusBody?.webhook);
ok('GET /grok/status redacts the sender key',
  statusRes.status === 200 && webhook?.key === 'set' && JSON.stringify(statusBody).includes(KEY) === false);

await replyNode.stop();
await grok.stop();
await rm(dir, { recursive: true, force: true });

if (failures > 0) {
  console.error(`grok-bot verify: ${failures} check(s) failed`);
  process.exit(1);
}
console.log('grok-bot verify: all checks passed');
