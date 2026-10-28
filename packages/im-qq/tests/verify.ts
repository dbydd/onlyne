import { deepStrictEqual } from 'node:assert/strict';
import { createQqConnector, createQqPort, QQ_OPERATIONS } from '../src/index.js';
import type { QqHttpRequest, QqHttpResponse, QqMessageBody, QqPort, QqPortStatus, QqSendResult, QqSendTarget } from '../src/port.js';
import { QqPortError } from '../src/port.js';
import { normalizeEvent } from '../src/normalize.js';

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

// ---------------------------------------------------------------------------
// normalization
// ---------------------------------------------------------------------------

const c2c = normalizeEvent({
  id: 'evt_1', op: 0, s: 1, t: 'C2C_MESSAGE_CREATE',
  d: {
    id: 'msg_1',
    author: { user_openid: 'openid_user', id: 'u1' },
    content: 'hello',
    timestamp: '2026-10-28T10:00:00+08:00',
    message_type: 0,
    message_scene: { source: 'default', ext: ['msg_idx=REFIDX_self'] },
    attachments: [],
  },
}, () => 0);
same('c2c: conversation, sender, body, handle, quote index', {
  platform: c2c?.platform,
  source_id: c2c?.source_id,
  conversation: c2c?.conversation,
  kind: c2c?.kind,
  from: c2c?.from,
  text: c2c?.text,
  attachments: c2c?.attachments,
  message_handle: c2c?.message_handle,
  ref_handle: c2c?.ref_handle,
  at: c2c?.at,
}, {
  platform: 'qq',
  source_id: 'msg_1',
  conversation: 'c2c:openid_user',
  kind: 'message',
  from: 'openid_user',
  text: 'hello',
  attachments: [],
  message_handle: 'msg_1',
  ref_handle: 'msg_1|REFIDX_self',
  at: '2026-10-28T10:00:00+08:00',
});

const group = normalizeEvent({
  id: 'evt_2', op: 0, t: 'GROUP_AT_MESSAGE_CREATE',
  d: {
    id: 'msg_2',
    group_openid: 'group_1',
    author: { user_openid: 'openid_ann' },
    content: '<@!123> ping',
    timestamp: '2026-10-28T10:00:01+08:00',
    message_type: 0,
    mentions: [{ id: '123', member_openid: 'openid_ann', username: 'Ann' }],
    attachments: [],
  },
}, () => 0);
same('group: conversation and sender', [group?.conversation, group?.from, group?.text], ['group:group_1', 'openid_ann', '<@!123> ping']);
same('group: mentions survive in the raw payload', (group?.raw as { d?: { mentions?: Array<{ member_openid?: string }> } } | undefined)?.d?.mentions?.[0]?.member_openid, 'openid_ann');
same('group: no quote index keeps the bare handle', group?.ref_handle, 'msg_2');

const quoted = normalizeEvent({
  id: 'evt_3', op: 0, t: 'C2C_MESSAGE_CREATE',
  d: {
    id: 'msg_3',
    author: { user_openid: 'openid_user' },
    content: '',
    message_type: 103,
    msg_elements: [{ content: 'the quoted words' }],
    message_scene: { ext: ['msg_idx=REFIDX_self', 'ref_msg_idx=REFIDX_parent'] },
    timestamp: '2026-10-28T10:00:02+08:00',
    attachments: [],
  },
}, () => 0);
same('quote reply: text from the quoted body', quoted?.text, 'the quoted words');
same('quote reply: parent and quoted_text', quoted?.reply, { parent: 'REFIDX_parent', quoted_text: 'the quoted words' });
same('quote reply: ref_handle pairs the two ids', quoted?.ref_handle, 'msg_3|REFIDX_self');

const ark = normalizeEvent({
  id: 'evt_4', op: 0, t: 'GROUP_AT_MESSAGE_CREATE',
  d: {
    id: 'msg_4',
    group_openid: 'group_1',
    author: { user_openid: 'openid_ann' },
    content: '',
    message_type: 3,
    ark_data: { prompt: '#card pick one' },
    timestamp: '2026-10-28T10:00:03+08:00',
    attachments: [],
  },
}, () => 0);
same('ark card: prompt becomes the text', ark?.text, '#card pick one');

const voice = normalizeEvent({
  id: 'evt_5', op: 0, t: 'C2C_MESSAGE_CREATE',
  d: {
    id: 'msg_5',
    author: { user_openid: 'openid_user' },
    content: '',
    message_type: 0,
    timestamp: '2026-10-28T10:00:04+08:00',
    attachments: [
      { content_type: 'voice', url: 'https://qq/voice.silk', voice_wav_url: 'https://qq/voice.wav', asr_refer_text: '转写文字', filename: 'voice.silk' },
      { content_type: 'image/png', url: 'https://qq/pic.png', filename: 'pic.png' },
    ],
  },
}, () => 0);
same('voice: transcript becomes the text', voice?.text, '转写文字');
same('voice: wav handle and audio kind; image keeps its url', voice?.attachments, [
  { kind: 'audio', platform_handle: 'https://qq/voice.wav', alt: 'voice.silk' },
  { kind: 'image', platform_handle: 'https://qq/pic.png', alt: 'pic.png' },
]);

const added = normalizeEvent({
  id: 'evt_add', op: 0, t: 'GROUP_ADD_ROBOT',
  d: { group_openid: 'group_1', op_member_openid: 'openid_ann', timestamp: '2026-10-28T10:00:05+08:00' },
}, () => 0);
same('lifecycle: kind event with the envelope id', {
  source_id: added?.source_id, conversation: added?.conversation, kind: added?.kind,
  event: added?.event, from: added?.from, text: added?.text,
}, {
  source_id: 'evt_add', conversation: 'group:group_1', kind: 'event',
  event: { type: 'GROUP_ADD_ROBOT' }, from: 'openid_ann', text: '',
});

const friend = normalizeEvent({ id: 'evt_friend', op: 0, t: 'FRIEND_ADD', d: { openid: 'openid_new', timestamp: '2026-10-28T10:00:06+08:00' } }, () => 0);
same('lifecycle: friend add is a c2c conversation', [friend?.conversation, friend?.from], ['c2c:openid_new', 'openid_new']);

const interaction = normalizeEvent({
  id: 'evt_click', op: 0, t: 'INTERACTION_CREATE',
  d: { id: 'action_1', group_openid: 'group_1', user_openid: 'openid_ann', data: { button: 'yes' }, timestamp: '2026-10-28T10:00:07+08:00' },
}, () => 0);
same('interaction: kind action with the platform id and value', {
  source_id: interaction?.source_id, kind: interaction?.kind, action: interaction?.action, conversation: interaction?.conversation,
}, {
  source_id: 'action_1', kind: 'action', action: { id: 'action_1', value: { button: 'yes' } }, conversation: 'group:group_1',
});

same('unknown event name is not translated', normalizeEvent({ id: 'e', op: 0, t: 'SOMETHING_NEW', d: {} }), undefined);
same('non-dispatch op is not an inbound event', normalizeEvent({ id: 'e', op: 13, d: { plain_token: 'x', event_ts: '1' } }), undefined);
same('a non-object payload is not an event', normalizeEvent('nope'), undefined);

const noTimestamp = normalizeEvent({
  id: 'evt_6', op: 0, t: 'C2C_MESSAGE_CREATE',
  d: { id: 'msg_6', author: { user_openid: 'openid_user' }, content: 'x', message_type: 0, attachments: [] },
}, () => Date.parse('2026-10-28T00:00:00Z'));
same('missing timestamp falls back to the injected clock', noTimestamp?.at, '2026-10-28T00:00:00.000Z');

// ---------------------------------------------------------------------------
// port: token cache, refresh window, rate limits, error shape
// ---------------------------------------------------------------------------

function jsonResponse(status: number, body: unknown): QqHttpResponse {
  return { status, text: async () => JSON.stringify(body) };
}

let clock = 1_700_000_000_000;
let tokenIssued = 0;
const calls: Array<{ url: string; init: QqHttpRequest }> = [];
const port = createQqPort({
  appId: 'app_1',
  appSecret: 'secret_1',
  now: () => clock,
  fetch: async (url, init) => {
    calls.push({ url, init });
    if (url.endsWith('/app/getAppAccessToken')) {
      tokenIssued += 1;
      return jsonResponse(200, { access_token: `token_${tokenIssued}`, expires_in: 7200 });
    }
    return jsonResponse(200, { id: 'msg_sent', ext_info: { ref_idx: 'REFIDX_out' } });
  },
});
await port.start();
const [firstToken, secondToken] = await Promise.all([port.token(), port.token()]);
same('concurrent token calls share one request', [firstToken, secondToken, tokenIssued], ['token_1', 'token_1', 1]);
same('token request body names appId and clientSecret', JSON.parse(calls[0]?.init.body ?? '{}'), { appId: 'app_1', clientSecret: 'secret_1' });

const sent = await port.send({ kind: 'c2c', openid: 'openid_user' }, { msg_type: 0, content: 'hi' });
same('send: platform path, bearer header and response id', {
  path: calls[1]?.url,
  authorization: calls[1]?.init.headers.authorization,
  contentType: calls[1]?.init.headers['content-type'],
  id: sent.id,
  ref: sent.ext_info?.ref_idx,
}, {
  path: 'https://api.bot.qq.com/v2/users/openid_user/messages',
  authorization: 'QQBot token_1',
  contentType: 'application/json',
  id: 'msg_sent',
  ref: 'REFIDX_out',
});

clock += 7_200_000 - 60_000 - 1;
await port.token();
same('a token inside the refresh window is reused', tokenIssued, 1);
clock += 2;
await port.token();
same('a token past the refresh window is refetched', tokenIssued, 2);
same('port status counts no rate limits yet', port.status(), { state: 'connected', connected: true, rate_limits: 0 });

const limited = createQqPort({
  appId: 'app_1',
  appSecret: 'secret_1',
  fetch: async () => jsonResponse(429, { code: 11244, message: 'too many requests' }),
});
let limitedError: unknown;
try {
  await limited.send({ kind: 'group', openid: 'group_1' }, { msg_type: 0, content: 'x' });
} catch (error) {
  limitedError = error;
}
ok('HTTP 429 throws a typed error', limitedError instanceof QqPortError);
same('HTTP 429 keeps status, code and platform text', {
  status: (limitedError as QqPortError).status,
  code: (limitedError as QqPortError).code,
  detail: (limitedError as QqPortError).detail,
}, { status: 429, code: 11244, detail: 'too many requests' });
same('HTTP 429 is counted for status', limited.status().rate_limits, 1);

const badSecret = createQqPort({
  appId: 'app_1',
  appSecret: 'wrong',
  fetch: async () => jsonResponse(200, { code: 100016, message: 'invalid appid or secret' }),
});
let secretError: unknown;
try {
  await badSecret.token();
} catch (error) {
  secretError = error;
}
same('a 2xx error envelope is a rejection, not a token', {
  isPortError: secretError instanceof QqPortError,
  code: (secretError as QqPortError | undefined)?.code,
}, { isPortError: true, code: 100016 });
same('a failed token leaves the port in error', badSecret.status().state, 'error');

// ---------------------------------------------------------------------------
// connector: onboarding, status, operations
// ---------------------------------------------------------------------------

class FakePort implements QqPort {
  tokenCalls = 0;
  stopped = false;
  rateLimits = 0;
  readonly requests: Array<{ method: string; path: string; body?: unknown }> = [];
  readonly sends: Array<{ target: QqSendTarget; body: QqMessageBody }> = [];
  answer: QqSendResult | Error = { id: 'sent_1' };

  async start(): Promise<void> {}
  async stop(): Promise<void> { this.stopped = true; }
  async token(): Promise<string> { this.tokenCalls += 1; return 'token'; }
  async request<T>(method: string, path: string, body?: unknown): Promise<T> {
    this.requests.push({ method, path, ...(body === undefined ? {} : { body }) });
    return {} as T;
  }
  async send(target: QqSendTarget, body: QqMessageBody): Promise<QqSendResult> {
    this.sends.push({ target, body });
    if (this.answer instanceof Error) throw this.answer;
    return this.answer;
  }
  status(): QqPortStatus { return { state: 'connected', connected: true, rate_limits: this.rateLimits }; }
}

const bare = createQqConnector({ env: {} });
await bare.start();
same('no credential is onboarding, not a failed start', {
  state: bare.status().state,
  connected: bare.status().connected,
  onboarding: bare.status().onboarding,
  reconnects: bare.status().reconnects,
  rate_limits: bare.status().rate_limits,
}, {
  state: 'onboarding', connected: false, onboarding: ['QQ_BOT_APP_ID', 'QQ_BOT_APP_SECRET'], reconnects: 0, rate_limits: 0,
});
const bareSend = await bare.send({ conversation: 'c2c:openid_user', text: 'hi', format: 'plain', attachments: [] });
ok('unconnected send is unknown with a not-connected error', bareSend.state === 'unknown' && /not connected/.test(bareSend.error ?? ''));
const bareOps = await bare.operate('recall', { conversation: 'c2c:openid_user', message_id: 'msg_1' });
same('unconnected operation is unknown', bareOps.state, 'unknown');

const malformed = createQqConnector({ credential: 'only-app-id', env: {} });
await malformed.start();
same('a credential without a secret is an error state', [malformed.status().state, malformed.status().error], ['error', 'The QQ credential is appId:appSecret.']);

const unreadable = createQqConnector({
  credentialFile: '/nonexistent/qq.credential',
  env: {},
  readFile: async () => { throw new Error('ENOENT'); },
});
await unreadable.start();
same('an unreadable credential file reports the resolve problem', {
  state: unreadable.status().state,
  onboarding: unreadable.status().onboarding,
}, {
  state: 'onboarding',
  onboarding: ['The credential file is unreadable: /nonexistent/qq.credential'],
});

const fake = new FakePort();
fake.rateLimits = 3;
const connector = createQqConnector({ credential: 'app_1:secret_1', port: fake, transport: 'webhook', env: {} });
await connector.start();
same('a resolved credential proves the token and connects', {
  state: connector.status().state,
  connected: connector.status().connected,
  tokenCalls: fake.tokenCalls,
  rate_limits: connector.status().rate_limits,
}, { state: 'connected', connected: true, tokenCalls: 1, rate_limits: 3 });
same('declared operations', [...connector.operations], [...QQ_OPERATIONS]);

let rejectedUnknown = false;
try {
  connector.validate('update_card', { conversation: 'c2c:x', message_id: 'm' });
} catch {
  rejectedUnknown = true;
}
ok('an undeclared operation is rejected', rejectedUnknown);
let rejectedRecall = false;
try {
  connector.validate('recall', { conversation: 'c2c:x' });
} catch {
  rejectedRecall = true;
}
ok('recall without message_id is rejected', rejectedRecall);
let acceptedTyping = true;
try {
  connector.validate('typing', { conversation: 'c2c:x' });
} catch {
  acceptedTyping = false;
}
ok('typing validates on conversation alone', acceptedTyping);

same('recall retracts through the group message path', {
  outcome: (await connector.operate('recall', { conversation: 'group:group_1', message_id: 'msg_9' })).state,
  call: fake.requests[0],
}, {
  outcome: 'committed',
  call: { method: 'DELETE', path: '/v2/groups/group_1/messages/msg_9' },
});

const typing = await connector.operate('typing', { conversation: 'c2c:openid_user' });
same('typing is a c2c input_notify send', {
  outcome: typing.state,
  target: fake.sends[0]?.target,
  body: fake.sends[0]?.body,
}, {
  outcome: 'committed',
  target: { kind: 'c2c', openid: 'openid_user' },
  body: { msg_type: 6, input_notify: { input_type: 1, input_second: 60 } },
});

const groupTyping = await connector.operate('typing', { conversation: 'group:group_1' });
same('group typing is refused without a platform call', [groupTyping.state, groupTyping.error, fake.sends.length], ['unknown', 'QQ typing is c2c only.', 1]);

const badConversation = await connector.operate('recall', { conversation: 'user:openid_user', message_id: 'msg_1' });
same('an unknown conversation scope is refused', badConversation.state, 'unknown');

await connector.stop();
same('stop clears the port', [fake.stopped, connector.status().state], [true, 'stopped']);

if (failures > 0) {
  console.error(`${failures} check(s) failed`);
  process.exit(1);
}
console.log('verify: all checks passed');
