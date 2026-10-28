import { deepStrictEqual } from 'node:assert/strict';
import type { ServerWebSocket } from 'bun';
import {
  createQqConnector, createQqGateway,
  type QqConnector,
} from '../src/index.js';
import type { QqMessageBody, QqPort, QqPortStatus, QqSendResult, QqSendTarget } from '../src/port.js';

let failures = 0;
function ok(label: string, condition: boolean): void {
  if (condition) {
    console.log(`ok ${label}`);
    return;
  }
  failures += 1;
  console.error(`not ok ${label}`);
}
function same(label: string, actual: unknown, expected: unknown): void {
  try {
    deepStrictEqual(actual, expected);
    ok(label, true);
  } catch {
    failures += 1;
    console.error(`not ok ${label}\n  actual:   ${JSON.stringify(actual)}\n  expected: ${JSON.stringify(expected)}`);
  }
}

type Payload = Record<string, unknown>;

/** The loopback gateway: a real socket the connector dials, not a mocked WebSocket object. */
interface Loopback {
  url: string;
  /** Client payloads, in arrival order, across every connection. */
  received: Payload[];
  /** Answers one client payload; the test decides what the platform says. */
  onClient?: (payload: Payload) => void;
  send(payload: Payload): void;
  /** Closes the current connection from the server side, as a network drop would. */
  drop(): void;
  stop(): void;
}

function loopback(heartbeatInterval = 5_000): Loopback {
  const received: Payload[] = [];
  let current: ServerWebSocket<unknown> | undefined;
  const api: Loopback = {
    url: '',
    received,
    send(payload) {
      current?.send(JSON.stringify(payload));
    },
    drop() {
      current?.close();
    },
    stop() {
      server.stop(true);
    },
  };
  const server = Bun.serve({
    port: 0,
    fetch(request, srv) {
      if (srv.upgrade(request)) return undefined;
      return new Response('upgrade required', { status: 400 });
    },
    websocket: {
      open(ws) {
        current = ws;
        ws.send(JSON.stringify({ op: 10, d: { heartbeat_interval: heartbeatInterval } }));
      },
      message(_ws, message) {
        const payload = JSON.parse(String(message)) as Payload;
        received.push(payload);
        api.onClient?.(payload);
      },
    },
  });
  api.url = `ws://127.0.0.1:${server.port}/websocket`;
  return api;
}

/** The HTTP boundary: the token, and `/gateway/bot` pointing at the loopback. */
class FakePort implements QqPort {
  gatewayUrl = '';
  shards = 1;
  readonly calls: string[] = [];
  limit = { max_concurrency: 1, remaining: 100, reset_after: 60_000, total: 100 };

  async start(): Promise<void> {}
  async stop(): Promise<void> {}
  async token(): Promise<string> {
    return 'token_1';
  }
  async request<T>(method: string, path: string): Promise<T> {
    this.calls.push(`${method} ${path}`);
    if (path !== '/gateway/bot') throw new Error(`unexpected ${method} ${path}`);
    return { url: this.gatewayUrl, shards: this.shards, session_start_limit: this.limit } as T;
  }
  async send(_target: QqSendTarget, _body: QqMessageBody): Promise<QqSendResult> {
    return { id: 'sent_1' };
  }
  status(): QqPortStatus {
    return { state: 'connected', connected: true, rate_limits: 0 };
  }
}

async function waitFor(label: string, predicate: () => boolean, timeoutMs = 3_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (predicate()) return;
    await Bun.sleep(5);
  }
  ok(label, false);
}

const ops = (loop: Loopback, op: number): Payload[] => loop.received.filter(payload => payload.op === op);
const body = (payload: Payload | undefined): Payload => (payload?.d ?? {}) as Payload;

const MESSAGE = {
  id: 'msg_1',
  content: 'hello',
  timestamp: '2026-10-28T10:00:00+08:00',
  author: { user_openid: 'openid_user' },
};

// ---------------------------------------------------------------------------
// Hello → Identify → READY, and a heartbeat the platform acknowledges
// ---------------------------------------------------------------------------

const first = loopback(200);
const firstPort = new FakePort();
firstPort.gatewayUrl = first.url;
first.onClient = payload => {
  if (payload.op === 1) first.send({ op: 11 });
};
const gateway = createQqGateway({ port: firstPort, reconnectBaseMs: 0 });
await gateway.start({ dispatch: () => {} });

await waitFor('the gateway sends Identify', () => ops(first, 2).length === 1);
same('Identify carries the bot token, the intents and the shard', {
  token: body(ops(first, 2)[0]).token,
  intents: body(ops(first, 2)[0]).intents,
  shard: body(ops(first, 2)[0]).shard,
  // Group and c2c messages are bit 25, interaction callbacks bit 26.
}, { token: 'QQBot token_1', intents: 100663296, shard: [0, 1] });
same('discovery went through the HTTP port', firstPort.calls, ['GET /gateway/bot']);
same('a socket without READY is still connecting', gateway.status().state, 'connecting');

first.send({ op: 0, s: 1, t: 'READY', d: { session_id: 'sess_1' } });
await waitFor('READY puts the gateway online', () => gateway.status().state === 'online');
same('READY leaves one online shard and no reconnect', {
  state: gateway.status().state, connected: gateway.status().reconnects, shards: gateway.status().shards,
}, { state: 'online', connected: 0, shards: 1 });

await waitFor('the gateway heartbeats', () => ops(first, 1).length >= 1);
same('the first heartbeat carries the READY sequence number', body(ops(first, 1)[0]), 1);
await Bun.sleep(450);
same('acknowledged heartbeats keep the socket online', {
  state: gateway.status().state, timeouts: gateway.status().heartbeat_timeouts,
}, { state: 'online', timeouts: 0 });

// ---------------------------------------------------------------------------
// A missed ACK is a dead connection
// ---------------------------------------------------------------------------

first.onClient = undefined;
await waitFor('an unanswered heartbeat is counted', () => gateway.status().heartbeat_timeouts === 1);
await waitFor('the dead socket is replaced', () => gateway.status().reconnects >= 1);
ok('the timeout closed the connection instead of drifting', gateway.status().heartbeat_timeouts >= 1);
await gateway.stop();
first.stop();
same('a stopped gateway reports stopped', gateway.status().state, 'stopped');

// ---------------------------------------------------------------------------
// A dropped socket resumes with the session id and the last accepted seq
// ---------------------------------------------------------------------------

const second = loopback();
const secondPort = new FakePort();
secondPort.gatewayUrl = second.url;
const delivered: number[] = [];
const resumed = createQqGateway({ port: secondPort, reconnectBaseMs: 0 });
await resumed.start({ dispatch: payload => { delivered.push(Number(payload.s)); } });
await waitFor('the second gateway identifies', () => ops(second, 2).length === 1);
second.send({ op: 0, s: 1, t: 'READY', d: { session_id: 'sess_2' } });
await waitFor('the second gateway is online', () => resumed.status().state === 'online');
second.send({ op: 0, s: 7, t: 'C2C_MESSAGE_CREATE', d: MESSAGE });
await waitFor('the dispatch reaches the listener', () => delivered.includes(7));

second.drop();
await waitFor('the dropped socket resumes', () => ops(second, 6).length === 1);
same('Resume names the session and the last accepted sequence', {
  token: body(ops(second, 6)[0]).token,
  session_id: body(ops(second, 6)[0]).session_id,
  seq: body(ops(second, 6)[0]).seq,
}, { token: 'QQBot token_1', session_id: 'sess_2', seq: 7 });
same('a resume in flight is the resuming state', resumed.status().state, 'resuming');

second.send({ op: 0, s: 7, t: 'RESUMED', d: {} });
await waitFor('RESUMED puts the gateway back online', () => resumed.status().state === 'online');
same('the resumed session is counted', [resumed.status().resumes, resumed.status().reconnects], [1, 1]);

// ---------------------------------------------------------------------------
// op 9 discards the session: the next attempt is a fresh Identify
// ---------------------------------------------------------------------------

second.send({ op: 9, d: false });
await waitFor('op 9 leads to a second Identify', () => ops(second, 2).length === 2);
same('op 9 never resumes the discarded session', ops(second, 6).length, 1);
same('the rejected session is counted', resumed.status().resume_failures, 1);
same('the fresh Identify still asks for one shard', body(ops(second, 2)[1]).shard, [0, 1]);
await resumed.stop();
second.stop();

// ---------------------------------------------------------------------------
// A listener failure is redelivered: the sequence number does not advance
// ---------------------------------------------------------------------------

const third = loopback();
const thirdPort = new FakePort();
thirdPort.gatewayUrl = third.url;
const attempts: number[] = [];
const accepted: number[] = [];
const replaying = createQqGateway({ port: thirdPort, reconnectBaseMs: 0 });
await replaying.start({
  dispatch: payload => {
    const seq = Number(payload.s);
    attempts.push(seq);
    if (seq === 2 && attempts.filter(entry => entry === 2).length === 1) throw new Error('listener failed');
    accepted.push(seq);
  },
});
await waitFor('the third gateway identifies', () => ops(third, 2).length === 1);
third.send({ op: 0, s: 1, t: 'READY', d: { session_id: 'sess_3' } });
await waitFor('the third gateway is online', () => replaying.status().state === 'online');
third.onClient = payload => {
  // The platform replays from the sequence number the resume names.
  if (payload.op === 6) third.send({ op: 0, s: 2, t: 'C2C_MESSAGE_CREATE', d: MESSAGE });
};
third.send({ op: 0, s: 2, t: 'C2C_MESSAGE_CREATE', d: MESSAGE });
await waitFor('the failed event is replayed', () => accepted.includes(2));
await waitFor('the replay resumes from the accepted sequence', () => ops(third, 6).length === 1);
same('the resume names the sequence before the failed event', body(ops(third, 6)[0]).seq, 1);
same('the listener saw the failed event twice and accepted it once', [attempts, accepted], [[2, 2], [2]]);
await replaying.stop();
third.stop();

// ---------------------------------------------------------------------------
// The connector: dispatch → normalizeEvent → onEvent, and a listener retry
// ---------------------------------------------------------------------------

// A fast heartbeat is how this test observes the sequence number the connector has accepted.
const fourth = loopback(100);
const fourthPort = new FakePort();
fourthPort.gatewayUrl = fourth.url;
const seen: string[] = [];
let failing = true;
let replays = 0;
const connector: QqConnector = createQqConnector({
  credential: 'app_1:secret_1',
  port: fourthPort,
  transport: 'websocket',
  env: {},
  log: () => {},
  now: () => Date.parse('2026-10-28T02:00:00Z'),
  gateway: { reconnectBaseMs: 0 },
});
connector.onEvent(event => {
  if (failing) {
    failing = false;
    throw new Error('receive failed');
  }
  seen.push(event.source_id);
});
fourth.onClient = payload => {
  if (payload.op === 1) fourth.send({ op: 11 });
  if (payload.op === 2) fourth.send({ op: 0, s: 1, t: 'READY', d: { session_id: 'sess_4' } });
  if (payload.op === 6 && replays === 0) {
    replays += 1;
    fourth.send({ op: 0, s: 1, t: 'RESUMED', d: {} });
    fourth.send({ op: 0, s: 2, t: 'C2C_MESSAGE_CREATE', d: MESSAGE });
  }
};
await connector.start();
await waitFor('the connector reaches the gateway', () => connector.status().state === 'online');
fourth.send({ op: 0, s: 2, t: 'C2C_MESSAGE_CREATE', d: MESSAGE });
await waitFor('the failed delivery comes back after the resume', () => seen.includes('msg_1'));
same('the event is normalized into one ImEvent', seen, ['msg_1']);
same('the accepted event is the one last_event_at reports', connector.status().last_event_at, '2026-10-28T10:00:00+08:00');
same('the connector reports the socket transport and its state', {
  state: connector.status().state, transport: connector.status().transport, shards: connector.status().shards,
}, { state: 'online', transport: 'websocket', shards: 1 });

// An event this connector does not translate is accepted, not replayed forever.
fourth.send({ op: 0, s: 3, t: 'SOMETHING_NEW', d: { id: 'evt_1' } });
await waitFor('an untranslated event advances the sequence', () => ops(fourth, 1).some(payload => payload.d === 3));
fourth.drop();
await waitFor('the next resume follows the untranslated event', () => ops(fourth, 6).length === 2);
same('the resume names the sequence of the untranslated event', body(ops(fourth, 6)[1]).seq, 3);

await connector.stop();
same('a stopped connector reports stopped', connector.status().state, 'stopped');
fourth.stop();

if (failures > 0) {
  console.error(`gateway: ${failures} check(s) failed`);
  process.exit(1);
}
console.log('gateway: all checks passed');
