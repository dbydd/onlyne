import { deepStrictEqual } from 'node:assert/strict';
import {
  botMemberEvent,
  cardActionEvent,
  messageEvent,
  reactionEvent,
  recallEvent,
  type NormalizeContext,
} from '../src/normalize.js';

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

const ctx: NormalizeContext = { botIdentity: { openId: 'ou_bot', name: 'Onlyne' } };

function messageRaw(message: Record<string, unknown>, senderOpenId = 'ou_sender'): unknown {
  return {
    sender: { sender_id: { open_id: senderOpenId }, sender_type: 'user' },
    message: {
      chat_id: 'oc_chat',
      chat_type: 'group',
      create_time: '1730000000000',
      ...message,
    },
  };
}

// --- text message: body, sender, dedup key, @ replacement ---
const textEvent = await messageEvent(
  messageRaw({
    message_id: 'om_text',
    message_type: 'text',
    content: JSON.stringify({ text: 'hello @_user_1, ping @_user_2' }),
    mentions: [
      { key: '@_user_1', id: { open_id: 'ou_tom' }, name: 'Tom' },
      { key: '@_user_2', id: { open_id: 'ou_bot' }, name: 'Onlyne' },
    ],
  }),
  ctx,
);
ok('text: an im.message.receive_v1 event normalizes', textEvent !== undefined);
ok('text: platform is lark', textEvent?.platform === 'lark');
ok('text: source_id is the message_id (dedup key)', textEvent?.source_id === 'om_text');
ok('text: conversation is the chat_id', textEvent?.conversation === 'oc_chat');
ok('text: kind is message', textEvent?.kind === 'message');
ok('text: from is the sender open_id', textEvent?.from === 'ou_sender');
ok('text: message_handle is the message_id', textEvent?.message_handle === 'om_text');
ok('text: @name replaced, bot mention stripped', textEvent?.text === 'hello @Tom, ping');
same('text: no attachments', textEvent?.attachments, []);
ok('text: a non-reply carries no reply coordinate', textEvent?.reply === undefined);
ok('text: at is the message create_time as ISO', textEvent?.at === '2024-10-27T03:33:20.000Z');

// --- post (rich text) message: body and inline resources ---
const postEvent = await messageEvent(
  messageRaw({
    message_id: 'om_post',
    message_type: 'post',
    content: JSON.stringify({
      zh_cn: {
        title: 'Weekly',
        content: [
          [
            { tag: 'text', text: 'hello ' },
            { tag: 'at', user_id: 'ou_tom' },
            { tag: 'text', text: ' see ' },
            { tag: 'a', text: 'link', href: 'https://example.com' },
            { tag: 'img', image_key: 'img_post' },
          ],
        ],
      },
    }),
    mentions: [{ key: '@_user_1', id: { open_id: 'ou_tom' }, name: 'Tom' }],
  }),
  ctx,
);
ok(
  'post: title, mention, link and image are flattened into the body',
  postEvent?.text === '**Weekly**\n\nhello @Tom see [link](https://example.com)![image](img_post)',
);
same('post: the inline image becomes an attachment', postEvent?.attachments, [
  { kind: 'image', platform_handle: 'img_post' },
]);

// --- image message ---
const imageEvent = await messageEvent(
  messageRaw({ message_id: 'om_image', message_type: 'image', content: JSON.stringify({ image_key: 'img_key_1' }) }),
  ctx,
);
same('image: image_key becomes an image attachment', imageEvent?.attachments, [
  { kind: 'image', platform_handle: 'img_key_1' },
]);
ok('image: body keeps the image reference', imageEvent?.text === '![image](img_key_1)');

// --- file message ---
const fileEvent = await messageEvent(
  messageRaw({
    message_id: 'om_file',
    message_type: 'file',
    content: JSON.stringify({ file_key: 'file_key_1', file_name: 'quarterly.pdf' }),
  }),
  ctx,
);
same('file: file_key and file_name become an attachment with alt', fileEvent?.attachments, [
  { kind: 'file', platform_handle: 'file_key_1', alt: 'quarterly.pdf' },
]);
ok('file: body keeps key and name', fileEvent?.text === '<file key="file_key_1" name="quarterly.pdf"/>');

// --- reply coordinates ---
const replyEvent = await messageEvent(
  messageRaw({
    message_id: 'om_reply',
    message_type: 'text',
    content: JSON.stringify({ text: 'answering' }),
    parent_id: 'om_parent',
    root_id: 'om_root',
    thread_id: 'omt_thread',
  }),
  ctx,
);
same('reply: parent, root and thread are carried', replyEvent?.reply, {
  parent: 'om_parent',
  root: 'om_root',
  thread: 'omt_thread',
});
ok('reply: source_id stays the reply message own id', replyEvent?.source_id === 'om_reply');

// --- a truncated payload is a state, not an exception ---
ok('message: a payload without sender/message yields undefined', (await messageEvent({}, ctx)) === undefined);

// --- card action ---
const cardRaw = {
  context: { open_message_id: 'om_card', open_chat_id: 'oc_card' },
  operator: { open_id: 'ou_op', name: 'Operator' },
  action: { tag: 'button', name: 'approve', value: { action: 'approve_btn', id: 'ticket-1' } },
};
const cardEvent = cardActionEvent(cardRaw);
const cardEventAgain = cardActionEvent(cardRaw);
ok('card: kind is action', cardEvent?.kind === 'action');
ok('card: conversation is the chat_id', cardEvent?.conversation === 'oc_card');
ok('card: from is the operator open_id', cardEvent?.from === 'ou_op');
ok('card: message_handle is the card message_id', cardEvent?.message_handle === 'om_card');
ok('card: text is empty and attachments empty', cardEvent?.text === '' && (cardEvent?.attachments.length ?? -1) === 0);
ok(
  'card: source_id is message:operator:tag|name|option|value',
  cardEvent?.source_id === 'card:om_card:ou_op:button|approve||{"action":"approve_btn","id":"ticket-1"}',
);
ok('card: the same button twice yields the same source_id', cardEvent?.source_id === cardEventAgain?.source_id);
same('card: action.value passes through untouched', cardEvent?.action?.value, { action: 'approve_btn', id: 'ticket-1' });
ok('card: action.id is value.action', cardEvent?.action?.id === 'approve_btn');
ok('card: at is an ISO timestamp of the receive time', typeof cardEvent?.at === 'string' && !Number.isNaN(Date.parse(cardEvent.at)));
ok('card: a malformed payload yields undefined', cardActionEvent({}) === undefined);

const stringValueCard = cardActionEvent({
  context: { open_message_id: 'om_card', open_chat_id: 'oc_card' },
  operator: { open_id: 'ou_op' },
  action: { tag: 'button', name: 'reject', value: 'reject_btn' },
});
ok(
  'card: a string value keys on the raw string and falls back to action.name for the id',
  stringValueCard?.source_id === 'card:om_card:ou_op:button|reject||reject_btn' && stringValueCard?.action?.id === 'reject',
);
ok(
  'card: a different button yields a different source_id',
  stringValueCard?.source_id !== cardEvent?.source_id,
);

// --- reaction ---
const reactionAdded = reactionEvent(
  {
    event_id: 'evt_reaction',
    message_id: 'om_reacted',
    reaction_type: { emoji_type: 'THUMBSUP' },
    operator_type: 'user',
    user_id: { open_id: 'ou_reactor' },
    action_time: '1730000005000',
  },
  'added',
  'oc_chat',
);
ok('reaction: source_id is the platform event_id', reactionAdded?.source_id === 'evt_reaction');
ok('reaction: kind is event', reactionAdded?.kind === 'event');
ok('reaction: conversation is the chat id the caller resolved', reactionAdded?.conversation === 'oc_chat');
ok('reaction: from is the operator open_id', reactionAdded?.from === 'ou_reactor');
same('reaction: event shape', reactionAdded?.event, {
  type: 'reaction.added',
  data: { message_id: 'om_reacted', emoji: 'THUMBSUP', operator: 'ou_reactor' },
});
ok('reaction: at is the action_time as ISO', reactionAdded?.at === '2024-10-27T03:33:25.000Z');
ok('reaction: no message_handle', reactionAdded?.message_handle === undefined);
ok('reaction: text empty and attachments empty', reactionAdded?.text === '' && reactionAdded?.attachments.length === 0);

const reactionRemoved = reactionEvent(
  { message_id: 'om_reacted', reaction_type: { emoji_type: 'THUMBSUP' }, user_id: { open_id: 'ou_reactor' } },
  'removed',
  'oc_chat',
);
ok(
  'reaction: without event_id the source_id is the synthesized reaction key',
  reactionRemoved?.source_id === 'rx:om_reacted:ou_reactor:THUMBSUP:removed:0',
);
ok('reaction: removed event type', reactionRemoved?.event?.type === 'reaction.removed');
ok('reaction: without action_time at falls back to now', Date.parse(reactionRemoved?.at ?? '') > Date.now() - 60_000);
ok('reaction: a malformed payload yields undefined', reactionEvent({}, 'added', 'oc_chat') === undefined);

// --- recall ---
const recall = recallEvent({
  event_id: 'evt_recall',
  message_id: 'om_recalled',
  chat_id: 'oc_recall',
  recall_time: '1730000010000',
  recall_type: 'user',
});
ok('recall: source_id is the platform event_id', recall?.source_id === 'evt_recall');
ok('recall: conversation is the chat_id', recall?.conversation === 'oc_recall');
ok('recall: kind is event, no body', recall?.kind === 'event' && recall?.text === '' && recall?.attachments.length === 0);
same('recall: event shape', recall?.event, {
  type: 'message.recalled',
  data: { message_id: 'om_recalled', recall_type: 'user' },
});
ok('recall: at is recall_time as ISO', recall?.at === '2024-10-27T03:33:30.000Z');
ok('recall: no message_handle', recall?.message_handle === undefined);

const recallSeconds = recallEvent({
  event_id: 'evt_recall_s',
  message_id: 'om_recalled',
  chat_id: 'oc_recall',
  recall_time: '1730000010',
});
ok('recall: a seconds-scale timestamp yields the same instant', recallSeconds?.at === '2024-10-27T03:33:30.000Z');
ok('recall: a payload without event_id yields undefined', recallEvent({ message_id: 'om_x', chat_id: 'oc_x' }) === undefined);

// --- bot added / removed ---
const botRaw = {
  event_id: 'evt_bot',
  chat_id: 'oc_bot',
  operator_id: { open_id: 'ou_admin' },
  external: false,
  name: 'Onlyne',
  create_time: '1730000020000',
};
const botAdded = botMemberEvent(botRaw, 'bot.added');
ok('bot: source_id is the platform event_id', botAdded?.source_id === 'evt_bot');
ok('bot: conversation is the chat_id', botAdded?.conversation === 'oc_bot');
ok('bot: from is the operator open_id', botAdded?.from === 'ou_admin');
same('bot: added event shape', botAdded?.event, {
  type: 'bot.added',
  data: { operator: 'ou_admin', external: false, name: 'Onlyne' },
});
ok('bot: at is create_time as ISO', botAdded?.at === '2024-10-27T03:33:40.000Z');
ok('bot: no message_handle', botAdded?.message_handle === undefined);

const botRemoved = botMemberEvent(botRaw, 'bot.removed');
ok('bot: removed event type on the same body', botRemoved?.event?.type === 'bot.removed');
ok('bot: a payload without event_id yields undefined', botMemberEvent({ chat_id: 'oc_x', operator_id: { open_id: 'ou_x' } }, 'bot.added') === undefined);

const passed = failures === 0;
console.log(`\n${passed ? 'all assertions passed' : `${failures} assertions failed`}`);
if (!passed) process.exit(1);
