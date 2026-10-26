import { createLarkConnector, createLarkPort } from '../src/index.js';
import type { LarkHandlers, LarkPort } from '../src/port.js';
import type { ImEvent } from '@onlyne/im-core';
import type { SendInput, SendOptions, SendResult } from '@larksuiteoapi/node-sdk';

let failures = 0;
const ok = (label: string, condition: boolean): void => {
  if (condition) console.log(`ok   ${label}`);
  else { failures += 1; console.error(`FAIL ${label}`); }
};

/** Waits for the connector's background work; events are emitted off the WS callback path. */
async function waitFor(predicate: () => boolean, label: string): Promise<void> {
  for (let attempt = 0; attempt < 200; attempt += 1) {
    if (predicate()) return;
    await new Promise(resolve => setTimeout(resolve, 5));
  }
  failures += 1;
  console.error(`FAIL timeout: ${label}`);
}

class FakePort implements LarkPort {
  handlers: LarkHandlers | undefined;
  started = false;
  stopped = false;
  readonly sends: Array<{ to: string; input: SendInput; opts?: SendOptions }> = [];
  readonly answers: Array<SendResult | Error> = [];
  readonly updates: Array<{ messageId: string; card: object }> = [];
  readonly recalls: string[] = [];
  readonly reactions: Array<{ messageId: string; emoji: string }> = [];
  readonly chats: Record<string, string> = {};
  readonly downloads: string[] = [];
  /** The next lifecycle call throws this, so the connector's rejection mapping is exercised. */
  rejectNext: Error | undefined;

  async start(handlers: LarkHandlers): Promise<void> { this.handlers = handlers; this.started = true; }
  async stop(): Promise<void> { this.stopped = true; }
  status(): { state: string } { return { state: this.started ? 'connected' : 'idle' }; }
  botIdentity(): { openId: string; name: string } { return { openId: 'ou_bot', name: 'Onlyne' }; }
  async fetchSubMessages(): Promise<unknown[]> { return []; }
  async messageChat(messageId: string): Promise<string | undefined> { return this.chats[messageId]; }
  async send(to: string, input: SendInput, opts?: SendOptions): Promise<SendResult> {
    this.sends.push({ to, input, ...(opts === undefined ? {} : { opts }) });
    const next = this.answers.shift();
    if (next === undefined) throw new Error('The test scripted no answer.');
    if (next instanceof Error) throw next;
    return next;
  }
  async updateCard(messageId: string, card: object): Promise<void> { this.updates.push({ messageId, card }); }
  async recallMessage(messageId: string): Promise<void> {
    if (this.rejectNext !== undefined) {
      const error = this.rejectNext;
      this.rejectNext = undefined;
      throw error;
    }
    this.recalls.push(messageId);
  }
  async addReaction(messageId: string, emoji: string): Promise<string> {
    this.reactions.push({ messageId, emoji });
    return 'rx_1';
  }
  async downloadResource(fileKey: string): Promise<Buffer> {
    this.downloads.push(fileKey);
    return Buffer.from('bytes');
  }
}

const messageRaw = (id: string, text: string): unknown => ({
  sender: { sender_id: { open_id: 'ou_sender' }, sender_type: 'user' },
  message: {
    message_id: id, chat_id: 'oc_chat', chat_type: 'group',
    create_time: '1730000000000', message_type: 'text', content: JSON.stringify({ text }),
  },
});

const port = new FakePort();
const errors: unknown[] = [];
const events: ImEvent[] = [];
const connector = createLarkConnector({ port, onError: error => errors.push(error) });
connector.onEvent(event => { events.push(event); });

await connector.start();
ok('start hands the handlers to the port', port.started && port.handlers !== undefined);

// --- inbound: message ---
port.handlers!.message(messageRaw('om_1', '第一条'));
port.handlers!.message(messageRaw('om_2', '第二条'));
await waitFor(() => events.length === 2, 'two messages emitted');
ok('messages keep arrival order', events[0]?.source_id === 'om_1' && events[1]?.source_id === 'om_2');
ok('a message carries its conversation and text', events[0]?.conversation === 'oc_chat' && events[0]?.text === '第一条');
ok('a message carries its own handle', events[0]?.message_handle === 'om_1');

// --- inbound: card action answers the callback before the work runs ---
const toast = port.handlers!.cardAction({
  context: { open_message_id: 'om_card', open_chat_id: 'oc_chat' },
  operator: { open_id: 'ou_clicker' },
  action: { tag: 'button', name: 'approve', value: { action: 'approve' } },
});
ok('a card action answers with a toast', (toast as { toast?: { type?: string } }).toast?.type === 'info');
await waitFor(() => events.length === 3, 'card action emitted');
ok('a card action becomes an action event', events[2]?.kind === 'action' && events[2]?.action?.id === 'approve');

// --- inbound: reaction needs the chat resolved from the message ---
port.chats['om_reacted'] = 'oc_chat';
port.handlers!.reaction({ event_id: 'ev_rx', message_id: 'om_reacted', reaction_type: { emoji_type: 'SMILE' }, user_id: { open_id: 'ou_reactor' }, action_time: '1730000001000' }, 'added');
await waitFor(() => events.length === 4, 'reaction emitted');
ok('a reaction is a lifecycle event', events[3]?.kind === 'event' && events[3]?.event?.type === 'reaction.added');
ok('a reaction carries the resolved chat', events[3]?.conversation === 'oc_chat');

// --- inbound: recall and bot added ---
port.handlers!.recall({ event_id: 'ev_recall', message_id: 'om_gone', chat_id: 'oc_chat', recall_time: '1730000002000' });
await waitFor(() => events.length === 5, 'recall emitted');
ok('a recall is a lifecycle event', events[4]?.event?.type === 'message.recalled');

port.handlers!.botMember({ event_id: 'ev_bot', chat_id: 'oc_new', operator_id: { open_id: 'ou_adder' }, name: 'Onlyne' }, 'bot.added');
await waitFor(() => events.length === 6, 'bot added emitted');
ok('the bot joining is a lifecycle event', events[5]?.event?.type === 'bot.added' && events[5]?.conversation === 'oc_new');

// --- outbound ---
port.answers.push({ messageId: 'om_out' });
const sent = await connector.send({
  conversation: 'oc_chat', text: '答复', format: 'plain', attachments: [], reply_to: 'om_in',
});
ok('send commits with the platform message id', sent.state === 'committed' && sent.message_id === 'om_out');
ok('send passes the reply handle as replyTo', port.sends[0]?.opts?.replyTo === 'om_in');
ok('send passes the body as text', (port.sends[0]?.input as { text?: string }).text === '答复');

// --- extra operations ---
ok('the connector declares its operations', (connector.operations ?? []).includes('update_card'));
connector.validate('update_card', { message_id: 'om_card', card: { schema: '2.0' } });
let rejected = false;
try { connector.validate('update_card', { card: {} }); } catch { rejected = true; }
ok('update_card without message_id is rejected', rejected);

const updated = await connector.operate('update_card', { message_id: 'om_card', card: { schema: '2.0' } });
ok('update_card commits', updated.state === 'committed' && port.updates.length === 1);
const recalled = await connector.operate('recall', { message_id: 'om_gone' });
ok('recall commits', recalled.state === 'committed' && port.recalls[0] === 'om_gone');
const reacted = await connector.operate('react', { message_id: 'om_1', emoji: 'SMILE' });
ok('react commits', reacted.state === 'committed' && port.reactions[0]?.emoji === 'SMILE');

port.rejectNext = Object.assign(new Error('permission denied'), { code: 'permission_denied' });
const refused = await connector.operate('recall', { message_id: 'om_nope' });
ok('a platform rejection settles as a rejection', refused.state === 'committed' && refused.error?.includes('permission denied') === true);

// --- a listener fault does not stop the stream ---
connector.onEvent(() => { throw new Error('listener fault'); });
port.handlers!.message(messageRaw('om_3', '第三条'));
await waitFor(() => events.length === 7, 'stream continues after a listener fault');
ok('a listener fault is reported', errors.length > 0);

await connector.stop();
ok('stop closes the port', port.stopped);

// --- the real SDK boundary instantiates without a network round trip ---
const real = createLarkPort({ appId: 'cli_test', appSecret: 'secret_test' });
ok('the real port starts idle', real.status().state === 'idle');
let identityRefused = false;
try { real.botIdentity(); } catch { identityRefused = true; }
ok('the real port has no identity before start', identityRefused);
await real.stop();

if (failures > 0) {
  console.error(`${failures} failure(s)`);
  process.exit(1);
}
console.log('im-lark verify: all checks passed');
