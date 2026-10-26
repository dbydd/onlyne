import {
  normalize,
  normalizeBotAdded,
  normalizeCardAction,
  normalizeReaction,
  type NormalizeOptions,
  type NormalizedMessage,
  type RawBotAddedEvent,
  type RawCardActionEvent,
  type RawMessageEvent,
  type RawReactionEvent,
} from '@larksuiteoapi/node-sdk';
import type { ImEvent, ImReply } from '@onlyne/im-core';
import type { Json } from '@onlyne/lib';

const PLATFORM = 'lark';

/** What the SDK's normalizers need from the connector: the bot's own identity, and merge-forward fetching. */
export interface NormalizeContext {
  botIdentity: { openId: string; name: string };
  fetchSubMessages?: (messageId: string) => Promise<unknown[]>;
}

/**
 * `im.message.receive_v1`. The SDK does the message-type work (text, post, media,
 * mentions, reply coordinates). `includeRaw` stays off: this signature has no such
 * switch, and the gateway already holds the platform's own event, so a copy here
 * would only double the payload.
 */
export async function messageEvent(raw: unknown, ctx: NormalizeContext): Promise<ImEvent | undefined> {
  if (!isMessageEvent(raw)) return undefined;
  const message = await normalize(raw, {
    botIdentity: ctx.botIdentity,
    fetchSubMessages: ctx.fetchSubMessages as NormalizeOptions['fetchSubMessages'],
  });
  const reply = replyOf(message);
  return {
    platform: PLATFORM,
    source_id: message.messageId,
    conversation: message.chatId,
    kind: 'message',
    from: message.senderId,
    text: message.content,
    attachments: message.resources.map(resource => ({
      kind: resource.type,
      platform_handle: resource.fileKey,
      ...(resource.fileName === undefined ? {} : { alt: resource.fileName }),
    })),
    ...(reply === undefined ? {} : { reply }),
    message_handle: message.messageId,
    at: new Date(message.createTime).toISOString(),
  };
}

/**
 * `card.action.trigger`. `source_id` reuses the SDK's own action key shape, so a
 * repeated click and a platform redelivery collapse to one event id. The SDK
 * reports no timestamp, so `at` is the receive time.
 */
export function cardActionEvent(raw: unknown): ImEvent | undefined {
  if (!isRecord(raw)) return undefined;
  const event = normalizeCardAction(raw as RawCardActionEvent);
  if (event === null) return undefined;
  const value = event.action.value;
  return {
    platform: PLATFORM,
    source_id: `card:${event.messageId}:${event.operator.openId}:${actionKey(event.action)}`,
    conversation: event.chatId,
    kind: 'action',
    from: event.operator.openId,
    text: '',
    attachments: [],
    action: { id: actionLabel(value, event.action), value: value as Json },
    message_handle: event.messageId,
    at: new Date().toISOString(),
  };
}

/**
 * `im.message.reaction.created_v1` / `deleted_v1`. The platform event carries no
 * chat id, so the caller passes the one it resolved from `message_id`.
 */
export function reactionEvent(raw: unknown, action: 'added' | 'removed', chatId: string): ImEvent | undefined {
  if (!isRecord(raw)) return undefined;
  const event = normalizeReaction(raw as RawReactionEvent, action);
  if (event === null) return undefined;
  return {
    platform: PLATFORM,
    source_id:
      eventIdOf(raw) ?? `rx:${event.messageId}:${event.operator.openId}:${event.emojiType}:${action}:${event.actionTime ?? 0}`,
    conversation: chatId,
    kind: 'event',
    from: event.operator.openId,
    text: '',
    attachments: [],
    event: {
      type: `reaction.${action}`,
      data: { message_id: event.messageId, emoji: event.emojiType, operator: event.operator.openId },
    },
    at: event.actionTime === undefined ? new Date().toISOString() : new Date(event.actionTime).toISOString(),
  };
}

/**
 * `im.message.recalled_v1`. The SDK has no normalizer for it. Without `event_id`
 * there is no dedup key, and a synthesized one would change on every redelivery,
 * so such an event is dropped instead of given a key that dedups nothing.
 */
export function recallEvent(raw: unknown): ImEvent | undefined {
  if (!isRecord(raw)) return undefined;
  const sourceId = eventIdOf(raw);
  const messageId = raw.message_id;
  const chatId = raw.chat_id;
  if (sourceId === undefined || typeof messageId !== 'string' || typeof chatId !== 'string') return undefined;
  return {
    platform: PLATFORM,
    source_id: sourceId,
    conversation: chatId,
    kind: 'event',
    text: '',
    attachments: [],
    event: {
      type: 'message.recalled',
      data: { message_id: messageId, recall_type: typeof raw.recall_type === 'string' ? raw.recall_type : null },
    },
    at: isoFromEpoch(raw.recall_time ?? raw.create_time),
  };
}

/** `im.chat.member.bot.added_v1` / `deleted_v1`: the same body, told apart by which event it arrived on. */
export function botMemberEvent(raw: unknown, type: 'bot.added' | 'bot.removed'): ImEvent | undefined {
  if (!isRecord(raw)) return undefined;
  const sourceId = eventIdOf(raw);
  if (sourceId === undefined) return undefined;
  const event = normalizeBotAdded(raw as RawBotAddedEvent);
  if (event === null) return undefined;
  return {
    platform: PLATFORM,
    source_id: sourceId,
    conversation: event.chatId,
    kind: 'event',
    from: event.operator.openId,
    text: '',
    attachments: [],
    event: {
      type,
      data: { operator: event.operator.openId, external: event.external ?? null, name: event.botName ?? null },
    },
    at: isoFromEpoch(raw.create_time),
  };
}

function replyOf(message: NormalizedMessage): ImReply | undefined {
  const reply: ImReply = { parent: message.replyToMessageId, root: message.rootId, thread: message.threadId };
  const empty = reply.parent === undefined && reply.root === undefined && reply.thread === undefined;
  return empty ? undefined : reply;
}

/** `tag|name|option|value`, the key the SDK's own dedup uses; the value part is capped at 128 characters. */
function actionKey(action: { tag: string; name?: string; option?: string; value?: unknown }): string {
  const valuePart = typeof action.value === 'string' ? action.value : JSON.stringify(action.value) ?? '';
  return `${action.tag}|${action.name ?? ''}|${action.option ?? ''}|${valuePart.slice(0, 128)}`;
}

/** The button's own action name: `value.action` when it is a string, else the element name, else the tag. */
function actionLabel(value: unknown, action: { tag: string; name?: string }): string {
  if (isRecord(value) && typeof value.action === 'string') return value.action;
  return action.name ?? action.tag;
}

/** Feishu sends epoch numbers as strings, in seconds on some events and milliseconds on others. */
function isoFromEpoch(value: unknown): string {
  const epoch = typeof value === 'number' ? value : typeof value === 'string' && value.trim() !== '' ? Number(value) : Number.NaN;
  if (!Number.isFinite(epoch) || epoch <= 0) return new Date().toISOString();
  return new Date(epoch <= 1e10 ? epoch * 1000 : epoch).toISOString();
}

function eventIdOf(raw: Record<string, unknown>): string | undefined {
  return typeof raw.event_id === 'string' && raw.event_id.length > 0 ? raw.event_id : undefined;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

/** The SDK reads `sender.sender_id` and `message.message_id` unguarded; a truncated payload must not throw. */
function isMessageEvent(raw: unknown): raw is RawMessageEvent {
  if (!isRecord(raw)) return false;
  const sender = raw.sender;
  const message = raw.message;
  if (!isRecord(sender) || !isRecord(sender.sender_id) || !isRecord(message)) return false;
  return typeof message.message_id === 'string' && typeof message.chat_id === 'string';
}
