import type { ImEvent } from '@onlyne/im-core';
import { WechatPortError, createWechatConnector } from '../src/index.js';
import { normalizeMessage } from '../src/normalize.js';
import type {
  WechatConfigResponse, WechatOutboundMessage, WechatPort, WechatUpdatesResponse,
} from '../src/port.js';

let failures = 0;
const ok = (label: string, condition: boolean): void => {
  if (condition) console.log(`ok   ${label}`);
  else { failures += 1; console.error(`FAIL ${label}`); }
};

async function waitFor(predicate: () => boolean, label: string): Promise<void> {
  for (let attempt = 0; attempt < 200; attempt += 1) {
    if (predicate()) return;
    await new Promise<void>(resolve => setTimeout(resolve, 5));
  }
  failures += 1;
  console.error(`FAIL timeout: ${label}`);
}

const clock = (): number => 1730000000000;

// --- inbound: the message shapes the iLink API delivers ---
const userText = normalizeMessage({
  seq: 7,
  message_id: 446,
  from_user_id: 'wxid_alice',
  to_user_id: 'bot',
  message_type: 1,
  create_time_ms: 1730000000123,
  context_token: 'token-1',
  item_list: [{ type: 1, text_item: { text: '你好' }, msg_id: 'm-1' }],
}, clock);
ok('a user text message becomes a message event', userText?.kind === 'message' && userText.text === '你好');
ok('the source id is the platform message id', userText?.source_id === '446');
ok('a direct message is addressed as user:<id>', userText?.conversation === 'user:wxid_alice');
ok('the sender is the platform user id', userText?.from === 'wxid_alice');
ok('the context token rides in ref_handle', userText?.ref_handle === 'ctx:token-1');
ok('the message handle is the platform message id', userText?.message_handle === '446');
ok('at comes from create_time_ms', userText?.at === new Date(1730000000123).toISOString());
ok('raw keeps the platform message', (userText?.raw as { seq?: number } | undefined)?.seq === 7);

const groupText = normalizeMessage({
  seq: 8,
  from_user_id: 'wxid_bob',
  group_id: 'group-1',
  message_type: 1,
  item_list: [{ type: 1, text_item: { text: 'hi' } }],
}, clock);
ok('a group message is addressed as group:<id>', groupText?.conversation === 'group:group-1');
ok('a message without create_time_ms uses the injected clock', groupText?.at === new Date(clock()).toISOString());
ok('a message without message_id falls back to seq', groupText?.source_id === '8');
ok('a seq-only message uses the seq as its handle', groupText?.message_handle === '8');
ok('a message without a context token carries no ref_handle', groupText?.ref_handle === undefined);

const voice = normalizeMessage({
  message_id: 'v-1',
  from_user_id: 'wxid_alice',
  message_type: 1,
  item_list: [{ type: 3, voice_item: { text: '语音转写', full_url: 'https://cdn/voice.silk' } }],
}, clock);
ok('a voice transcript becomes the text', voice?.text === '语音转写');
ok('a voice message keeps the audio attachment', voice?.attachments[0]?.kind === 'audio' && voice?.attachments[0]?.platform_handle === 'https://cdn/voice.silk');

const media = normalizeMessage({
  message_id: 'media-1',
  from_user_id: 'wxid_alice',
  message_type: 1,
  item_list: [
    { type: 2, image_item: { encrypt_query_param: 'query-image' } },
    { type: 4, file_item: { full_url: 'https://cdn/doc.pdf', file_name: 'doc.pdf', len: 1024 } },
    { type: 5, video_item: { full_url: 'https://cdn/clip.mp4' } },
  ],
}, clock);
ok('an image without a url keeps the encrypted reference', media?.attachments[0]?.kind === 'image' && media?.attachments[0]?.platform_handle === 'query-image');
ok('a file attachment carries its name', media?.attachments[1]?.kind === 'file' && media?.attachments[1]?.alt === 'doc.pdf');
ok('a video is an attachment', media?.attachments[2]?.kind === 'video');
ok('a media-only message has empty text', media?.text === '');

const reply = normalizeMessage({
  message_id: 'reply-1',
  from_user_id: 'wxid_alice',
  message_type: 1,
  item_list: [{ type: 1, text_item: { text: 'yes' }, ref_msg: { msg_id: 'm-1' } }],
}, clock);
ok('a quoted message becomes the reply parent', reply?.reply?.parent === 'm-1');

ok('a bot echo is skipped', normalizeMessage({
  message_id: 'b-1', from_user_id: 'bot', message_type: 2, item_list: [{ type: 1, text_item: { text: 'echo' } }],
}, clock) === undefined);
ok('a message with no sender is skipped', normalizeMessage({ message_id: 'x-1', message_type: 1 }, clock) === undefined);
ok('a message with no id is skipped', normalizeMessage({ from_user_id: 'wxid_alice', message_type: 1 }, clock) === undefined);

// --- onboarding: a missing token is a state, not a start failure ---
const onboard = createWechatConnector({ env: {} });
await onboard.start();
const onboarding = onboard.status();
ok('a missing token is an onboarding state', onboarding.state === 'onboarding' && onboarding.connected === false);
ok('onboarding names the exact field', onboarding.onboarding?.length === 1 && onboarding.onboarding[0] === 'WECHAT_ILINK_BOT_TOKEN');
ok('onboarding is not reported as an error', onboarding.error === undefined);
const refused = await onboard.send({ conversation: 'user:wxid_alice', text: 'x', format: 'plain', attachments: [] });
ok('a send without a session is unknown, not fake success', refused.state === 'unknown' && refused.error?.includes('not connected') === true);

const unreadable = createWechatConnector({
  credentialFile: '/nope/token',
  readFile: async () => { throw new Error('missing'); },
});
await unreadable.start();
ok('an unreadable credential file is reported verbatim', unreadable.status().onboarding?.[0]?.includes('unreadable') === true);

// --- status and the declared operations, against an injected port ---
class FakePort implements WechatPort {
  readonly cursors: string[] = [];
  readonly typing: Array<{ userId: string; ticket: string; status: number }> = [];
  readonly updates: WechatUpdatesResponse[] = [];
  readonly log: string[] = [];
  config: WechatConfigResponse = { ret: 0, typing_ticket: 'ticket-1' };
  configError: Error | undefined;
  started = false;
  stopped = false;
  aborted = false;
  rateLimitCount = 0;

  async getUpdates(cursor: string, signal?: AbortSignal): Promise<WechatUpdatesResponse> {
    this.cursors.push(cursor);
    const next = this.updates.shift();
    if (next !== undefined) return next;
    // The real call is a long poll: it answers when the platform has news, or on shutdown.
    return new Promise<WechatUpdatesResponse>(resolve => {
      const done = (): void => { this.aborted = true; resolve({ ret: 0, msgs: [] }); };
      if (signal?.aborted === true) { done(); return; }
      signal?.addEventListener('abort', done, { once: true });
    });
  }

  async sendMessage(): Promise<{ ret: number }> { return { ret: 0 }; }

  async getConfig(): Promise<WechatConfigResponse> {
    this.log.push('getconfig');
    const failure = this.configError;
    if (failure !== undefined) {
      this.configError = undefined;
      throw failure;
    }
    return this.config;
  }

  async sendTyping(userId: string, typingTicket: string, status: 1 | 2): Promise<void> {
    this.log.push('sendtyping');
    this.typing.push({ userId, ticket: typingTicket, status });
  }

  async notifyStart(): Promise<void> { this.started = true; }
  async notifyStop(): Promise<void> { this.stopped = true; }
  rateLimits(): number { return this.rateLimitCount; }
}

const port = new FakePort();
port.updates.push({
  ret: 0,
  msgs: [{ message_id: 'in-1', from_user_id: 'wxid_alice', message_type: 1, create_time_ms: 1730000001000, item_list: [{ type: 1, text_item: { text: 'in' } }] }],
  get_updates_buf: 'buf-1',
});

const events: ImEvent[] = [];
const connector = createWechatConnector({ port, sleep: async () => undefined });
connector.onEvent(event => { events.push(event); });
await connector.start();
ok('start tells the platform the bot is up', port.started);
ok('start reports connecting before the first poll answers', connector.status().state === 'connecting');
ok('the connector declares only typing', JSON.stringify(connector.operations) === '["typing"]');

await waitFor(() => events.length === 1, 'one inbound event emitted');
ok('an inbound message is emitted', events[0]?.source_id === 'in-1');
ok('an emitted event updates last_event_at', connector.status().last_event_at === new Date(1730000001000).toISOString());
ok('the poll adopts the returned cursor', port.cursors[1] === 'buf-1');
ok('the connector reports polling once a batch is accepted', connector.status().state === 'polling' && connector.status().connected === true);
ok('status counts no rate limits', connector.status().rate_limits === 0);
ok('status counts no reconnects yet', connector.status().reconnects === 0);

connector.validate('typing', { conversation: 'user:wxid_alice' });
let rejectedCoordinate = false;
try { connector.validate('typing', { conversation: 'oc_1' }); } catch { rejectedCoordinate = true; }
ok('typing rejects a conversation without a coordinate', rejectedCoordinate);

let rejectedMissing = false;
try { connector.validate('typing', {}); } catch { rejectedMissing = true; }
ok('typing without a conversation is an invalid request', rejectedMissing);

let rejectedUndeclared = false;
try { connector.validate('recall', { conversation: 'user:wxid_alice' }); } catch { rejectedUndeclared = true; }
ok('an undeclared operation is rejected', rejectedUndeclared);

const typing = await connector.operate('typing', { conversation: 'group:group-1' });
ok('typing commits', typing.state === 'committed' && port.typing[0]?.status === 1);
ok('typing uses the group id as the platform user id', port.typing[0]?.userId === 'group-1');
ok('typing asks for the ticket before sending', port.log.join(',') === 'getconfig,sendtyping' && port.typing[0]?.ticket === 'ticket-1');

port.configError = new WechatPortError('The platform rejected the getconfig call: session expired', { ret: -14 });
const paused = await connector.operate('typing', { conversation: 'user:wxid_alice' });
ok('a platform answer settles the typing operation', paused.state === 'committed' && paused.error?.includes('session expired') === true);

port.configError = new WechatPortError('The platform call failed: 500 upstream', { status: 500 });
const unclear = await connector.operate('typing', { conversation: 'user:wxid_alice' });
ok('a transport failure leaves typing unknown', unclear.state === 'unknown');

port.config = { ret: 0 };
const noTicket = await connector.operate('typing', { conversation: 'user:wxid_alice' });
ok('a config answer without a ticket is not claimed as sent', noTicket.state === 'unknown' && noTicket.error?.includes('typing_ticket') === true);

await connector.stop();
ok('stop reports stopped', connector.status().state === 'stopped' && connector.status().connected === false);
ok('stop tells the platform', port.stopped);
ok('stop aborts the long poll', port.aborted);

if (failures > 0) {
  console.error(`${failures} failure(s)`);
  process.exit(1);
}
console.log('im-wechat verify: all checks passed');
