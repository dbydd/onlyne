import { createWechatPort } from '../src/index.js';
import { sendMessage } from '../src/send.js';
import type { WechatOutboundMessage } from '../src/port.js';

let failures = 0;
const ok = (label: string, condition: boolean): void => {
  if (condition) console.log(`ok   ${label}`);
  else { failures += 1; console.error(`FAIL ${label}`); }
};

interface Recorded {
  url: string;
  headers: Record<string, string>;
  body: Record<string, unknown>;
}

const requests: Recorded[] = [];
const answers: Array<{ status: number; body: string }> = [];
const fetchStub = async (input: string, init: RequestInit): Promise<Response> => {
  requests.push({
    url: input,
    headers: init.headers as Record<string, string>,
    body: JSON.parse(String(init.body)) as Record<string, unknown>,
  });
  const next = answers.shift() ?? { status: 200, body: '{"ret":0}' };
  return new Response(next.body, { status: next.status });
};

/** `0.5 * 2^32` is 2147483648, so the uin header is pinned without touching the clock. */
const uin = Buffer.from('2147483648', 'utf8').toString('base64');
const port = createWechatPort({ token: 'tok-1', fetch: fetchStub, random: () => 0.5, timeoutMs: 0 });

// --- a text send carries the documented envelope ---
const sent = await sendMessage(port, {
  conversation: 'user:wxid_alice',
  text: '你好',
  format: 'plain',
  attachments: [],
  request_id: 'req-1',
  reply_to: 'ctx:token-9',
});
ok('a text send commits with the client id as the message id', sent.state === 'committed' && sent.message_id === 'req-1');
ok('a context-token send carries no note', sent.notes === undefined && sent.error === undefined);

const request = requests[0]!;
ok('the send goes to sendmessage', request.url === 'https://ilinkai.weixin.qq.com/ilink/bot/sendmessage');
ok('the call is authorized as a bot token', request.headers.Authorization === 'Bearer tok-1' && request.headers.AuthorizationType === 'ilink_bot_token');
ok('the app id header is bot', request.headers['iLink-App-Id'] === 'bot');
ok('the client version header is the decimal form of 0x00010000', request.headers['iLink-App-ClientVersion'] === '65536');
ok('the uin header is a base64 uint32', request.headers['X-WECHAT-UIN'] === uin);
ok('the body is json', request.headers['Content-Type'] === 'application/json');

const msg = request.body.msg as WechatOutboundMessage;
ok('the message envelope is the documented one', msg.from_user_id === '' && msg.to_user_id === 'wxid_alice');
ok('a user send is a bot message in the sent state', msg.message_type === 2 && msg.message_state === 2);
ok('the client id comes from the request id', msg.client_id === 'req-1');
ok('the context token is carried verbatim', msg.context_token === 'token-9');
ok('the text is one text item', msg.item_list.length === 1 && msg.item_list[0]?.type === 1 && msg.item_list[0]?.text_item.text === '你好');

const baseInfo = request.body.base_info as { channel_version?: string; bot_agent?: string };
ok('base_info names the channel version and agent', baseInfo.channel_version === '3.0.0' && baseInfo.bot_agent === 'Onlyne');

// --- group sends address the group id ---
await sendMessage(port, { conversation: 'group:group-1', text: 'hi', format: 'plain', attachments: [] });
const groupMessage = requests[1]!.body.msg as WechatOutboundMessage;
ok('a group send uses the group id as to_user_id', groupMessage.to_user_id === 'group-1');
ok('a send without reply_to omits context_token', !('context_token' in groupMessage));
ok('a send without request_id invents a client id', /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(groupMessage.client_id));

// --- refusals that never reach the platform ---
const before = requests.length;
const quoted = await sendMessage(port, {
  conversation: 'user:wxid_alice', text: 'x', format: 'plain', attachments: [], reply_to: 'm-1',
});
ok('a quote request is refused', quoted.state === 'committed' && quoted.error?.startsWith('unsupported by @onlyne/im-wechat:') === true);
ok('a refused quote names the missing field', quoted.error?.includes('no message-quoting field') === true);
ok('a refused quote makes no platform call', requests.length === before);
ok('a refusal does not claim a platform reason', quoted.error?.includes('platform rejected') !== true && quoted.error?.includes('platform call failed') !== true);

const withAttachment = await sendMessage(port, {
  conversation: 'user:wxid_alice', text: '', format: 'plain', attachments: [{ kind: 'image', path: '/tmp/x.png' }],
});
ok('an attachment is refused', withAttachment.state === 'committed' && withAttachment.error?.startsWith('unsupported by @onlyne/im-wechat:') === true);
ok('an attachment send makes no platform call', requests.length === before);

const withCard = await sendMessage(port, {
  conversation: 'user:wxid_alice', text: '', format: 'plain', attachments: [], card: { schema: '2.0' },
});
ok('a card is refused', withCard.state === 'committed' && withCard.error?.startsWith('unsupported by @onlyne/im-wechat:') === true);
ok('a card send makes no platform call', requests.length === before);

const badConversation = await sendMessage(port, { conversation: 'wxid_alice', text: 'x', format: 'plain', attachments: [] });
ok('a conversation without a coordinate is refused', badConversation.state === 'committed' && badConversation.error?.startsWith('unsupported by @onlyne/im-wechat:') === true);

// --- platform answers and transport failures ---
answers.push({ status: 200, body: '{"ret":-1,"errmsg":"invalid token"}' });
const rejected = await sendMessage(port, { conversation: 'user:wxid_alice', text: 'x', format: 'plain', attachments: [] });
ok('a platform rejection settles the send', rejected.state === 'committed');
ok('a platform rejection quotes the platform text', rejected.error === 'The platform rejected the send: invalid token');

answers.push({ status: 500, body: 'upstream down' });
const unclear = await sendMessage(port, { conversation: 'user:wxid_alice', text: 'x', format: 'plain', attachments: [] });
ok('an HTTP failure leaves the send unknown', unclear.state === 'unknown');
ok('an HTTP failure names status and body', unclear.error === 'The platform call failed: 500 upstream down');

answers.push({ status: 429, body: 'slow down' });
const limited = await sendMessage(port, { conversation: 'user:wxid_alice', text: 'x', format: 'plain', attachments: [] });
ok('a 429 leaves the send unknown', limited.state === 'unknown' && limited.error?.startsWith('The platform rate-limited the send: 429') === true);
ok('a 429 is counted as a rate limit', port.rateLimits() === 1);

const offline = createWechatPort({
  token: 'tok-1',
  timeoutMs: 0,
  fetch: async () => { throw new Error('ECONNREFUSED'); },
});
const failed = await sendMessage(offline, { conversation: 'user:wxid_alice', text: 'x', format: 'plain', attachments: [] });
ok('a network failure leaves the send unknown', failed.state === 'unknown' && failed.error === 'The platform call failed: ECONNREFUSED');

if (failures > 0) {
  console.error(`${failures} failure(s)`);
  process.exit(1);
}
console.log('im-wechat send: all checks passed');
