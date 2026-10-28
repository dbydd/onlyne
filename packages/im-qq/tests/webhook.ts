import { deepStrictEqual } from 'node:assert/strict';
import { setTimeout as delay } from 'node:timers/promises';
import { createQqConnector, type QqConnector } from '../src/index.js';
import type { QqMessageBody, QqPort, QqPortStatus, QqSendResult, QqSendTarget } from '../src/port.js';
import { challenge, deriveEd25519, signHex, verifyHex } from '../src/webhook.js';

let failures = 0;
function ok(label: string, condition: boolean): void {
  if (condition) console.log(`ok   ${label}`);
  else {
    failures += 1;
    console.error(`FAIL ${label}`);
  }
}
function same(label: string, actual: unknown, expected: unknown): void {
  try {
    deepStrictEqual(actual, expected);
    ok(label, true);
  } catch {
    failures += 1;
    console.error(`FAIL ${label}\n  actual:   ${JSON.stringify(actual)}\n  expected: ${JSON.stringify(expected)}`);
  }
}

/** The two vectors QQ publishes for callback authentication. */
const SECRET = 'DG5g3B4j9X2KOErG';
const VECTOR_MESSAGE = '1725442341Arq0D5A61EgUu4OxUvOp';
const VECTOR_SIGNATURE = '87befc99c42c651b3aac0278e71ada338433ae26fcb24307bdc5ad38c1adc2d01bcfcadc0842edac85e85205028a1132afe09280305f13aa6909ffc2d652c706';
const PUBLIC_KEY_SECRET = 'naOC0ocQE3shWLAfffVLB1rhYPG7';
const PUBLIC_KEY_HEX = 'd7c362fe78aef81ff23287b493628b5db02a3c4fe30b215e4d19609b5d76673a';

same('the derived public key matches the published vector', deriveEd25519(PUBLIC_KEY_SECRET).publicHex, PUBLIC_KEY_HEX);
same('the challenge signature matches the published vector', signHex(SECRET, VECTOR_MESSAGE), VECTOR_SIGNATURE);
same('the published signature verifies', verifyHex(SECRET, VECTOR_MESSAGE, VECTOR_SIGNATURE), true);
same('a tampered message does not verify', verifyHex(SECRET, `${VECTOR_MESSAGE}!`, VECTOR_SIGNATURE), false);
same('a signature from another secret does not verify', verifyHex(PUBLIC_KEY_SECRET, VECTOR_MESSAGE, VECTOR_SIGNATURE), false);
same('a malformed signature is refused instead of throwing', verifyHex(SECRET, VECTOR_MESSAGE, 'zz'), false);
same('the key pair is stable across calls', deriveEd25519(SECRET).publicHex, deriveEd25519(SECRET).publicHex);

const validationBody = JSON.stringify({ op: 13, d: { plain_token: 'Arq0D5A61EgUu4OxUvOp', event_ts: '1725442341' } });
same('op 13 answers the token with its signature', challenge(SECRET, validationBody), {
  plain_token: 'Arq0D5A61EgUu4OxUvOp',
  signature: VECTOR_SIGNATURE,
});
same('op 13 without a token is not a challenge', challenge(SECRET, JSON.stringify({ op: 13, d: { event_ts: '1' } })), undefined);
same('a dispatch body is not a challenge', challenge(SECRET, JSON.stringify({ op: 0, d: {} })), undefined);
same('a non-JSON body is not a challenge', challenge(SECRET, 'not json'), undefined);

class FakePort implements QqPort {
  async start(): Promise<void> {}
  async stop(): Promise<void> {}
  async token(): Promise<string> { return 'token'; }
  async request<T>(): Promise<T> { return {} as T; }
  async send(_target: QqSendTarget, _body: QqMessageBody): Promise<QqSendResult> { return { id: 'msg_out' }; }
  status(): QqPortStatus { return { state: 'connected', connected: true, rate_limits: 0 }; }
}

const APP_ID = 'app_1';
const clock = Date.parse('2026-10-28T02:00:00Z');
const stamp = String(Math.floor(clock / 1000));

function connectorWith(listener?: (sourceId: string) => void | Promise<void>): QqConnector {
  const connector = createQqConnector({
    credential: `${APP_ID}:${SECRET}`,
    port: new FakePort(),
    env: {},
    now: () => clock,
  });
  if (listener !== undefined) connector.onEvent(event => listener(event.source_id));
  return connector;
}

function dispatch(messageId: string, content: string): string {
  return JSON.stringify({
    id: `evt_${messageId}`,
    op: 0,
    s: 1,
    t: 'C2C_MESSAGE_CREATE',
    d: {
      id: messageId,
      author: { user_openid: 'openid_user' },
      content,
      timestamp: '2026-10-28T10:00:00+08:00',
      message_type: 0,
      attachments: [],
    },
  });
}

function post(body: string, headers: Record<string, string>): Request {
  return new Request('http://127.0.0.1/onlyne/v3/webhook', {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...headers },
    body,
  });
}

function signed(body: string, overrides: Record<string, string> = {}): Request {
  return post(body, {
    'x-signature-ed25519': signHex(SECRET, `${stamp}${body}`),
    'x-signature-timestamp': stamp,
    ...overrides,
  });
}

const received: string[] = [];
const connector = connectorWith(sourceId => { received.push(sourceId); });
await connector.start();

const accepted = await connector.handleWebhook(signed(dispatch('msg_100', 'ping')));
same('a signed dispatch is acknowledged', [accepted.status, await accepted.json()], [200, { op: 12 }]);
same('the listener saw the event', received, ['msg_100']);
same('an accepted event updates last_event_at', connector.status().last_event_at, '2026-10-28T10:00:00+08:00');

const nonAscii = dispatch('msg_utf8', '你好，机器人');
const utf8 = await connector.handleWebhook(signed(nonAscii));
same('a UTF-8 body verifies against its own bytes', [utf8.status, received.at(-1)], [200, 'msg_utf8']);

const tamperedBody = dispatch('msg_101', 'ping');
const tampered = await connector.handleWebhook(post(tamperedBody, {
  'x-signature-ed25519': signHex(SECRET, `${stamp}${dispatch('msg_101', 'pong')}`),
  'x-signature-timestamp': stamp,
}));
same('a tampered body is refused', [tampered.status, (await tampered.json() as { error: { code: string } }).error.code], [401, 'unauthenticated']);
ok('a refused body never reaches the listener', !received.includes('msg_101'));

const staleStamp = String(Math.floor(clock / 1000) - 3600);
const staleBody = dispatch('msg_102', 'late');
const stale = await connector.handleWebhook(post(staleBody, {
  'x-signature-ed25519': signHex(SECRET, `${staleStamp}${staleBody}`),
  'x-signature-timestamp': staleStamp,
}));
same('a stale timestamp is refused even with a valid signature', stale.status, 401);

const unsigned = await connector.handleWebhook(post(dispatch('msg_103', 'x'), {}));
same('a dispatch without signature headers is refused', unsigned.status, 401);

const wrongAppId = await connector.handleWebhook(post(validationBody, { 'x-bot-appid': 'app_other' }));
same('a validation callback for another app is refused', wrongAppId.status, 401);
const rightAppId = await connector.handleWebhook(post(validationBody, { 'x-bot-appid': APP_ID }));
same('a validation callback for this app is answered', [rightAppId.status, await rightAppId.json()], [200, {
  plain_token: 'Arq0D5A61EgUu4OxUvOp',
  signature: VECTOR_SIGNATURE,
}]);

const wrongMethod = await connector.handleWebhook(new Request('http://127.0.0.1/onlyne/v3/webhook', { method: 'GET' }));
same('a non-POST callback is refused', wrongMethod.status, 405);

const unknownOp = await connector.handleWebhook(post(JSON.stringify({ op: 2, d: {} }), {}));
same('an op this connector does not translate is acknowledged', [unknownOp.status, await unknownOp.json()], [200, { op: 12 }]);

const unknownEvent = await connector.handleWebhook(signed(JSON.stringify({ id: 'evt_x', op: 0, t: 'FUTURE_EVENT', d: {} })));
same('an unknown event name is acknowledged without a delivery', [unknownEvent.status, received.length], [200, 2]);

// --- a listener failure must not be acked ---
const failing = connectorWith(() => { throw new Error('receive failed'); });
await failing.start();
const failingResponse = await failing.handleWebhook(signed(dispatch('msg_104', 'x')));
same('a failed listener answers 500 so the platform retries', failingResponse.status, 500);
same('a failed delivery does not move last_event_at', failing.status().last_event_at, undefined);

// --- ordering inside one conversation ---
const order: string[] = [];
let active = 0;
let peak = 0;
const ordered = connectorWith(async sourceId => {
  active += 1;
  peak = Math.max(peak, active);
  order.push(`start:${sourceId}`);
  if (sourceId === 'msg_a') await delay(20);
  order.push(`end:${sourceId}`);
  active -= 1;
});
await ordered.start();
const first = ordered.handleWebhook(signed(dispatch('msg_a', 'one')));
await delay(0);
const second = ordered.handleWebhook(signed(dispatch('msg_b', 'two')));
await Promise.all([first, second]);
same('events in one conversation reach the listener in arrival order', order, ['start:msg_a', 'end:msg_a', 'start:msg_b', 'end:msg_b']);
same('one listener call runs at a time per conversation', peak, 1);

// --- an unconfigured connector cannot validate a callback ---
const unconfigured = createQqConnector({ env: {} });
await unconfigured.start();
const unconfiguredResponse = await unconfigured.handleWebhook(post(validationBody, {}));
same('an unconfigured connector refuses callbacks instead of trusting them', unconfiguredResponse.status, 503);

await connector.stop();
const afterStop = await connector.handleWebhook(signed(dispatch('msg_105', 'x')));
same('a stopped connector still answers the platform', afterStop.status, 200);
same('a stopped connector delivers to nobody', received.includes('msg_105'), false);

if (failures > 0) {
  console.error(`${failures} check(s) failed`);
  process.exit(1);
}
console.log('webhook: all checks passed');
