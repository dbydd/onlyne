/**
 * Signed HTTP integration: two OnlyneNode instances over /onlyne/v3, discovery seeds,
 * trust exchange, grok-bot thread landing, reply, startNodeHost, and TUI/no-TUI.
 */
import { generateKeyMaterial, MemoryStorage, OnlyneError, OnlyneNode, trustPhase, type Auth, type KeyMaterial, type Plugins } from '@onlyne/lib';
import { createLogger, startNodeHost } from '@onlyne/node-host';
import {
  createGrokBotNode, grokBotThreadId, GrokBotPullClient, nodeClock, SERVE_IDLE_TIMEOUT_SEC,
  type FetchLike, type GrokBotConfig,
} from '../src/index.ts';

let failures = 0;
const ok = (label: string, condition: boolean): void => {
  if (condition) console.log(`ok - ${label}`);
  else { failures++; console.error(`not ok - ${label}`); }
};

const settle = (ms = 30): Promise<void> => new Promise(resolve => setTimeout(resolve, ms));
const waitFor = async (predicate: () => boolean | Promise<boolean>, timeoutMs = 5_000): Promise<boolean> => {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await predicate()) return true;
    await settle(20);
  }
  return false;
};

const rec = (value: unknown): Record<string, unknown> | undefined =>
  value !== null && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : undefined;

const GROK = 'urn:onlyne:gateway:grok-bot';
const ALICE = 'urn:onlyne:agent:alice';
const KEY = 'http-integration-sender-key';
const aliceDm = grokBotThreadId({ kind: 'dm', chat_id: ALICE });

const servers: Array<{ stop(closeActiveConnections?: boolean): void }> = [];
const closers: Array<() => Promise<void>> = [];

function keysHook(): Plugins['keys'] {
  let material: KeyMaterial = generateKeyMaterial();
  return Object.assign(async () => material, {
    replace: async (next: KeyMaterial) => { material = next; },
  });
}

function serve(fetch: (request: Request) => Promise<Response>): {
  url: string; port: number; set(next: (request: Request) => Promise<Response>): void;
} {
  let handler = fetch;
  const server = Bun.serve({
    hostname: '127.0.0.1',
    port: 0,
    idleTimeout: SERVE_IDLE_TIMEOUT_SEC,
    fetch: request => handler(request),
  });
  servers.push(server);
  const port = server.port ?? 0;
  return {
    url: `http://127.0.0.1:${port}/onlyne/v3`,
    port,
    set: next => { handler = next; },
  };
}

const posts: Array<Record<string, unknown>> = [];
const mockFetch: FetchLike = async (_input, init) => {
  const parsed = rec(JSON.parse(String(init?.body ?? '{}')));
  posts.push(parsed ?? {});
  return new Response(null, { status: 204 });
};

const grokBind = serve(async () => new Response('starting', { status: 503 }));
const aliceBind = serve(async () => new Response('starting', { status: 503 }));

const grokConfig: GrokBotConfig = {
  node: { id: GROK },
  server: { bind: '127.0.0.1', port: grokBind.port },
  storage_path: '/tmp/grok-bot-http-unused.json',
  webhook: {
    url: 'http://127.0.0.1:9/wake',
    key: KEY,
    header: 'Authorization',
    scheme: 'Bearer',
    timeout_ms: 200,
    max_attempts: 2,
    retry_base_ms: 1,
  },
  reply_path: '/grok/reply',
  events_path: '/grok/events',
  ack_path: '/grok/events/ack',
};

const grok = await createGrokBotNode(grokConfig, {
  storage: 'memory', keys: keysHook(), fetch: mockFetch,
});
grokBind.set(request => grok.fetch(request));
grok.node.client.runMaintenance();
closers.push(async () => { grok.node.client.stopMaintenance(); await grok.stop(); });

const aliceStorage = new MemoryStorage();
const aliceBase = aliceBind.url;
let aliceNode!: OnlyneNode;
const alicePlugins: Omit<Plugins, 'storage'> = {
  authenticate: async request => {
    const actor = request.headers.get('Onlyne-Actor');
    if (actor === null || actor.length === 0) return null;
    return { actor };
  },
  maintenanceClock: nodeClock(),
  resolve: async id => {
    const target = id === ALICE
      ? { node_id: id, base_url: aliceBase }
      : await aliceNode.discovery.resolve(id);
    return { ...target, headers: { 'Onlyne-Actor': ALICE } };
  },
  keys: keysHook(),
  decide: async () => { throw new OnlyneError('decision_unavailable', 'no decide'); },
  inboundThread: async () => true,
  runtime: {
    supports: async () => true,
    notify: () => {},
    inject: async () => ({ context: ALICE, consumed: true, outcome: 'delivered' as const }),
  },
};
aliceNode = new OnlyneNode({
  node_id: ALICE,
  base_url: aliceBase,
  authentication: ['signature'],
  plugins: { ...alicePlugins, storage: aliceStorage },
  now: () => Date.now(),
  id: () => crypto.randomUUID(),
});
aliceBind.set(request => aliceNode.fetch(request));
aliceNode.client.runMaintenance();
closers.push(async () => { aliceNode.client.stopMaintenance(); });

const grokAuth: Auth = { actor: GROK };
const aliceAuth: Auth = { actor: ALICE };

const observed = await aliceNode.discovery.observe(grokBind.url, 'bootstrap');
ok('alice observes the grok-bot service description as a seed',
  observed.node_id === GROK && observed.base_url === grokBind.url);

const grokObserved = await grok.node.discovery.observe(aliceBind.url, 'bootstrap');
ok('grok-bot observes the peer from a seed URL',
  grokObserved.node_id === ALICE);

const grokId = (await grok.node.keys.describe())!;
const aliceId = (await aliceNode.keys.describe())!;
ok('both nodes publish identity keys',
  grokId.fingerprint.length === 16 && aliceId.fingerprint.length === 16);

await aliceNode.trust.request(aliceAuth, GROK, grokId.fingerprint);
await grok.node.trust.accept(grokAuth, ALICE, aliceId.fingerprint);
await aliceNode.trust.accept(aliceAuth, GROK, grokId.fingerprint);
ok('the handshake reaches exchanged on both sides',
  trustPhase((await aliceNode.trust.entry(GROK))!) === 'exchanged'
  && trustPhase((await grok.node.trust.entry(ALICE))!) === 'exchanged');

const sent = rec(await aliceNode.transport.request(GROK, 'POST', '/messages', {
  idempotency_key: crypto.randomUUID(),
  retry_until: new Date(Date.now() + 60_000).toISOString(),
  message: { id: 'http-01', sender: ALICE, delivery_kind: 'note', msg: 'hello over signed HTTP' },
  target_endpoint: GROK,
  plan: { response: 'none', execution: 'existing' },
}));
ok('a signed POST /messages is admitted on grok-bot',
  rec(sent?.thread_ref)?.id === aliceDm && sent?.seq === '1' && sent?.thread_created === true);
ok('the signed message lands on grokbot:dm:<alice>',
  rec(sent?.thread_ref)?.id === aliceDm && rec(sent?.thread_ref)?.owner === GROK);

const continued = await aliceNode.client.send(GROK, {
  id: 'http-02', sender: ALICE, delivery_kind: 'note', msg: 'second over the grok-bot thread',
  meta: { thread: { owner: GROK, id: aliceDm } },
}, { response: 'none', execution: 'existing' });
ok('client.send onto the grok-bot thread is admitted',
  continued.admission === 'accepted'
  && rec(rec(continued.receipt)?.thread_ref)?.id === aliceDm);

ok('the webhook-equivalent payload is queued for the NAT puller',
  await waitFor(async () => (await grok.pull.snapshot()).owed + (await grok.pull.snapshot()).leased >= 1));
const puller = new GrokBotPullClient({
  origin: `http://127.0.0.1:${grokBind.port}`,
  webhook: grok.config.webhook,
  wait_ms: 0,
  lease_ms: 30_000,
});
const pulled = await puller.pull({ wait_ms: 0 });
const wake = pulled.find(item => rec(item.payload)?.message_id === 'http-01');
ok('GET /grok/events over HTTP returns the stable thread and grokbot routing meta',
  wake !== undefined
  && rec(rec(wake?.payload)?.thread)?.id === aliceDm
  && rec(rec(rec(wake?.payload)?.meta)?.grokbot)?.chat_kind === 'dm');
if (wake !== undefined) {
  const acked = await puller.ack([wake.id]);
  ok('POST /grok/events/ack over HTTP records the delivery', acked.acked.includes(wake.id));
}

ok('the cloud webhook path still fired for the same inbound',
  posts.some(body => body.message_id === 'http-01'));

const listed = await grok.node.messages.list(grokAuth, { owner: GROK, id: aliceDm }, {
  attention: 'thread', page_size: 50,
});
ok('grok-bot stored the inbound on the DM thread',
  listed.items.some(entry => entry.message.id === 'http-01'));

const reply = await grok.fetch(new Request('http://127.0.0.1/grok/reply', {
  method: 'POST',
  headers: { 'content-type': 'application/json', Authorization: `Bearer ${KEY}` },
  body: JSON.stringify({
    to: GROK,
    id: 'http-01-ack',
    msg: 'ack over the same thread',
    thread: { owner: GROK, id: aliceDm },
    in_reply_to: { sender: ALICE, id: 'http-01' },
  }),
}));
const replyBody = rec(JSON.parse(await reply.text()));
ok('a reply that echoes thread + in_reply_to is accepted',
  reply.status === 201 && replyBody?.admission === 'accepted'
  && rec(rec(replyBody?.receipt)?.thread_ref)?.id === aliceDm);

await grok.node.client.maintain();
await waitFor(async () => {
  await aliceNode.client.reconcile();
  const inbox = await aliceNode.messages.inbox(aliceAuth);
  return inbox.items.some(row => row.attention.message_ref.id === 'http-01-ack');
}, 5_000);
const aliceInbox = await aliceNode.messages.inbox(aliceAuth);
ok('alice receives the reply as a member of the grok-bot thread',
  aliceInbox.items.some(row => row.attention.message_ref.id === 'http-01-ack'));

// --- startNodeHost + TUI/no-TUI ---------------------------------------------

const hostLogs: string[] = [];
const logger = createLogger(true, 20);
const previousNoTui = process.env.ONLYNE_NO_TUI;

process.env.ONLYNE_NO_TUI = '1';
const hostNoTui = await startNodeHost({
  node: grok.node,
  endpoint: grokBind.url,
  bind: '127.0.0.1',
  seeds: [aliceBind.url],
  ring: logger.ring,
  interactive: true,
  log: (message, fields) => { hostLogs.push(message); logger.log(message, fields); },
  status: () => grok.statusLines(),
});
ok('ONLYNE_NO_TUI=1 does not start the trust terminal',
  hostLogs.every(line => line !== 'onlyne.tui.starting' && line !== 'onlyne.tui.ready'));
await hostNoTui.close();

delete process.env.ONLYNE_NO_TUI;
hostLogs.length = 0;
const hostQuiet = await startNodeHost({
  node: grok.node,
  endpoint: grokBind.url,
  bind: '127.0.0.1',
  interactive: false,
  ring: logger.ring,
  log: message => { hostLogs.push(message); },
});
ok('interactive:false skips the TUI even when a log ring exists',
  hostLogs.every(line => line !== 'onlyne.tui.starting'));
await hostQuiet.close();

hostLogs.length = 0;
const hostNoRing = await startNodeHost({
  node: grok.node,
  endpoint: grokBind.url,
  bind: '127.0.0.1',
  interactive: true,
  log: message => { hostLogs.push(message); },
});
ok('a non-TTY process without a ring never opens the TUI',
  hostLogs.every(line => line !== 'onlyne.tui.starting')
  && !(process.stdin.isTTY && process.stdout.isTTY && hostLogs.includes('onlyne.tui.ready')));
await hostNoRing.close();

if (previousNoTui === undefined) delete process.env.ONLYNE_NO_TUI;
else process.env.ONLYNE_NO_TUI = previousNoTui;

ok('this environment is non-TTY so a live TUI session is not exercised',
  process.stdin.isTTY !== true || process.stdout.isTTY !== true);

// --- real-socket long-poll past Bun's 10s default idleTimeout ----------------

const longBind = serve(async () => new Response('starting', { status: 503 }));
const longGrok = await createGrokBotNode({
  ...grokConfig,
  server: { bind: '127.0.0.1', port: longBind.port },
  storage_path: '/tmp/grok-bot-http-long.json',
}, { storage: 'memory', keys: keysHook(), fetch: mockFetch });
longBind.set(request => longGrok.fetch(request));
closers.push(async () => { longGrok.node.client.stopMaintenance(); await longGrok.stop(); });
const waitMs = 12_000;
const longStarted = Date.now();
let longOk = false;
let longElapsed = 0;
let longStatus = 0;
let longItems: unknown[] = [];
const longUrl = `http://127.0.0.1:${longBind.port}/grok/events?wait_ms=${waitMs}`;
const curlPath = Bun.which('curl');
try {
  if (curlPath !== null) {
    // curl is the production reproduction (code 000 / no body when idleTimeout cuts the socket).
    const bodyFile = `/tmp/grok-bot-long-poll-${crypto.randomUUID()}.json`;
    const proc = Bun.spawn([
      curlPath, '-sS', '-o', bodyFile, '-w', '%{http_code} %{time_total}',
      '--max-time', '20', '-H', `Authorization: Bearer ${KEY}`, longUrl,
    ], { stdout: 'pipe', stderr: 'pipe' });
    const [out, err, exit] = await Promise.all([
      new Response(proc.stdout).text(),
      new Response(proc.stderr).text(),
      proc.exited,
    ]);
    longElapsed = Date.now() - longStarted;
    const parts = out.trim().split(/\s+/);
    longStatus = Number(parts[0] ?? 0);
    if (exit === 0 && longStatus === 200) {
      const raw = await Bun.file(bodyFile).text();
      const longBody = rec(JSON.parse(raw.length === 0 ? '{}' : raw));
      longItems = Array.isArray(longBody?.items) ? longBody.items : [];
      longOk = true;
    } else {
      console.error(`long-poll curl exit ${exit} status ${longStatus} after ${longElapsed}ms stderr=${err} stdout=${out}`);
    }
  } else {
    const longRes = await fetch(longUrl, { headers: { Authorization: `Bearer ${KEY}` } });
    longElapsed = Date.now() - longStarted;
    longStatus = longRes.status;
    const longBody = rec(JSON.parse(await longRes.text()));
    longItems = Array.isArray(longBody?.items) ? longBody.items : [];
    longOk = true;
  }
} catch (error) {
  longElapsed = Date.now() - longStarted;
  console.error(`long-poll socket error after ${longElapsed}ms: ${error instanceof Error ? error.message : String(error)}`);
}
ok('an empty GET /grok/events wait_ms=12000 completes over a real socket',
  longOk && longStatus === 200 && Array.isArray(longItems) && longItems.length === 0);
ok('the empty long-poll waits past Bun.serve\'s default 10s idleTimeout',
  longOk && longElapsed >= 11_000 && longElapsed < waitMs + 5_000);

// --- pinned fingerprint pairing without the TUI ------------------------------

const pinPeerBind = serve(async () => new Response('starting', { status: 503 }));
const pinGrokBind = serve(async () => new Response('starting', { status: 503 }));
const pinPeerStorage = new MemoryStorage();
let pinPeer!: OnlyneNode;
const pinPeerPlugins: Omit<Plugins, 'storage'> = {
  authenticate: async request => {
    const actor = request.headers.get('Onlyne-Actor');
    return actor === null || actor.length === 0 ? null : { actor };
  },
  maintenanceClock: nodeClock(),
  resolve: async id => {
    const target = id === ALICE ? { node_id: id, base_url: pinPeerBind.url } : await pinPeer.discovery.resolve(id);
    return { ...target, headers: { 'Onlyne-Actor': ALICE } };
  },
  keys: keysHook(),
  decide: async () => { throw new OnlyneError('decision_unavailable', 'no decide'); },
  inboundThread: async () => true,
  runtime: { supports: async () => true, notify: () => {}, inject: async () => ({ context: ALICE, consumed: true, outcome: 'delivered' as const }) },
};
pinPeer = new OnlyneNode({
  node_id: ALICE,
  base_url: pinPeerBind.url,
  authentication: ['signature'],
  plugins: { ...pinPeerPlugins, storage: pinPeerStorage },
  now: () => Date.now(),
  id: () => crypto.randomUUID(),
});
pinPeerBind.set(request => pinPeer.fetch(request));
pinPeer.client.runMaintenance();
closers.push(async () => { pinPeer.client.stopMaintenance(); });
const pinPeerId = (await pinPeer.keys.describe())!;
const pinGrok = await createGrokBotNode({
  ...grokConfig,
  server: { bind: '127.0.0.1', port: pinGrokBind.port },
  storage_path: '/tmp/grok-bot-http-pin.json',
  trust_seeds: [{ node_id: ALICE, fingerprint: pinPeerId.fingerprint }],
}, { storage: 'memory', keys: keysHook(), fetch: mockFetch });
pinGrokBind.set(request => pinGrok.fetch(request));
pinGrok.node.client.runMaintenance();
closers.push(async () => { pinGrok.node.client.stopMaintenance(); await pinGrok.stop(); });
await pinPeer.discovery.observe(pinGrokBind.url, 'bootstrap');
await pinGrok.node.discovery.observe(pinPeerBind.url, 'bootstrap');
const pinGrokId = (await pinGrok.node.keys.describe())!;
await pinPeer.trust.request({ actor: ALICE }, GROK, pinGrokId.fingerprint);
ok('a pinned fingerprint auto-approves an inbound exchange without the TUI',
  trustPhase((await pinPeer.trust.entry(GROK))!) === 'exchanged'
  && trustPhase((await pinGrok.node.trust.entry(ALICE))!) === 'exchanged');

const WRONG = 'urn:onlyne:gateway:grok-wrong';
const wrongGrokBind = serve(async () => new Response('starting', { status: 503 }));
const wrongGrok = await createGrokBotNode({
  ...grokConfig,
  node: { id: WRONG },
  server: { bind: '127.0.0.1', port: wrongGrokBind.port },
  storage_path: '/tmp/grok-bot-http-wrong-pin.json',
  trust_seeds: [{ node_id: ALICE, fingerprint: 'aaaaaaaaaaaaaaaa' }],
}, { storage: 'memory', keys: keysHook(), fetch: mockFetch });
wrongGrokBind.set(request => wrongGrok.fetch(request));
wrongGrok.node.client.runMaintenance();
closers.push(async () => { wrongGrok.node.client.stopMaintenance(); await wrongGrok.stop(); });
await pinPeer.discovery.observe(wrongGrokBind.url, 'bootstrap');
await wrongGrok.node.discovery.observe(pinPeerBind.url, 'bootstrap');
const wrongId = (await wrongGrok.node.keys.describe())!;
let pinMismatch = false;
try {
  await pinPeer.trust.request({ actor: ALICE }, WRONG, wrongId.fingerprint);
} catch (error) {
  pinMismatch = error instanceof OnlyneError;
}
const wrongPhase = (await wrongGrok.node.trust.entry(ALICE));
ok('a pinned fingerprint that does not match is refused, not auto-accepted',
  pinMismatch && (wrongPhase === undefined || trustPhase(wrongPhase) !== 'exchanged'));

// --- webhook retry against a real failing HTTP endpoint ----------------------

let wakeStatus = 503;
let wakeHits = 0;
const wakeServer = Bun.serve({
  hostname: '127.0.0.1',
  port: 0,
  idleTimeout: SERVE_IDLE_TIMEOUT_SEC,
  fetch: () => {
    wakeHits += 1;
    return new Response(null, { status: wakeStatus });
  },
});
servers.push(wakeServer);
const retryGrokBind = serve(async () => new Response('starting', { status: 503 }));
const retryGrok = await createGrokBotNode({
  ...grokConfig,
  server: { bind: '127.0.0.1', port: retryGrokBind.port },
  storage_path: '/tmp/grok-bot-http-retry.json',
  webhook: {
    ...grokConfig.webhook,
    url: `http://127.0.0.1:${wakeServer.port}/wake`,
    timeout_ms: 200,
    max_attempts: 6,
    retry_base_ms: 20,
  },
}, { storage: 'memory', keys: keysHook() });
retryGrokBind.set(request => retryGrok.fetch(request));
closers.push(async () => { retryGrok.node.client.stopMaintenance(); await retryGrok.stop(); });
await retryGrok.node.messages.admit({ actor: ALICE }, {
  idempotency_key: crypto.randomUUID(),
  retry_until: new Date(Date.now() + 60_000).toISOString(),
  message: { id: 'retry-01', sender: ALICE, delivery_kind: 'note', msg: 'retry me' },
  target_endpoint: GROK,
  plan: { response: 'none', execution: 'existing' },
});
ok('a real 503 webhook is retried', await waitFor(() => wakeHits >= 1, 2_000));
wakeStatus = 204;
ok('webhook retry against a real endpoint eventually delivers', await waitFor(async () => {
  await retryGrok.maintain();
  const snap = await retryGrok.dispatcher.snapshot();
  return snap.queue.delivered >= 1;
}, 3_000));

for (const close of closers.splice(0)) await close();
for (const server of servers.splice(0)) server.stop(true);

if (failures > 0) {
  console.error(`grok-bot http: ${failures} check(s) failed`);
  process.exit(1);
}
console.log('grok-bot http: all checks passed');
