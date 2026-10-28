import type { ImAttachment, ImEvent, ImReply } from '@onlyne/im-core';
import type { Json } from '@onlyne/lib';
import type { WechatMediaRef, WechatMessage, WechatMessageItem } from './port.js';

const PLATFORM = 'wechat';

/** Item type to attachment kind: 2 image, 3 voice, 4 file, 5 video. */
const ATTACHMENT_KINDS: Record<number, ImAttachment['kind']> = {
  2: 'image',
  3: 'audio',
  4: 'file',
  5: 'video',
};

/**
 * One `WeixinMessage` from `getupdates`. `message_type === 2` is the bot's own outbound echo and
 * never becomes an inbound event; a message with no sender or no id cannot be deduplicated, so it
 * is dropped rather than given a synthetic id that would collide on the next poll.
 *
 * The conversation coordinate is `group:<group_id>` when the message names a group, else
 * `user:<from_user_id>`. The inbound `context_token` rides in `ref_handle` as `ctx:<token>`: the
 * bridge hands it back as `reply_to` on the answer to this event, and iLink wants it verbatim on
 * the send. `message_handle` stays the platform message id.
 */
export function normalizeMessage(message: WechatMessage, now: () => number): ImEvent | undefined {
  if (message.message_type === 2) return undefined;
  const from = message.from_user_id;
  if (typeof from !== 'string' || from.length === 0) return undefined;
  const id = message.message_id ?? message.seq;
  if (id === undefined || id === null || String(id).length === 0) return undefined;

  const items = message.item_list ?? [];
  const texts: string[] = [];
  let transcript: string | undefined;
  const attachments: ImAttachment[] = [];
  let parent: string | undefined;
  for (const item of items) {
    if (item.type === 1 && typeof item.text_item?.text === 'string') texts.push(item.text_item.text);
    if (transcript === undefined && item.type === 3) {
      const voice = item.voice_item?.text;
      if (typeof voice === 'string' && voice.length > 0) transcript = voice;
    }
    const attachment = attachmentOf(item);
    if (attachment !== undefined) attachments.push(attachment);
    if (parent === undefined) {
      const quoted = item.ref_msg?.msg_id;
      if (typeof quoted === 'string' && quoted.length > 0) parent = quoted;
    }
  }

  const text = texts.length > 0 ? texts.join('\n') : transcript ?? '';
  const reply: ImReply | undefined = parent === undefined ? undefined : { parent };
  const groupId = message.group_id;
  const conversation = typeof groupId === 'string' && groupId.length > 0 ? `group:${groupId}` : `user:${from}`;
  const contextToken = message.context_token;
  const createdAt = message.create_time_ms;

  return {
    platform: PLATFORM,
    source_id: String(id),
    conversation,
    kind: 'message',
    from,
    text,
    attachments,
    ...(reply === undefined ? {} : { reply }),
    ...(message.message_id === undefined ? {} : { message_handle: String(message.message_id) }),
    ...(typeof contextToken === 'string' && contextToken.length > 0 ? { ref_handle: `ctx:${contextToken}` } : {}),
    at: typeof createdAt === 'number' && Number.isFinite(createdAt) ? new Date(createdAt).toISOString() : new Date(now()).toISOString(),
    raw: message as unknown as Json,
  };
}

/** The media handle a later download needs: the pre-signed URL first, the encrypted reference second. */
function attachmentOf(item: WechatMessageItem): ImAttachment | undefined {
  const kind = item.type === undefined ? undefined : ATTACHMENT_KINDS[item.type];
  if (kind === undefined) return undefined;
  const media: WechatMediaRef | undefined = item.type === 2 ? item.image_item
    : item.type === 3 ? item.voice_item
      : item.type === 4 ? item.file_item
        : item.video_item;
  const handle = firstNonEmpty(media?.full_url) ?? firstNonEmpty(media?.encrypt_query_param);
  if (handle === undefined) return undefined;
  const name = item.type === 4 ? firstNonEmpty(item.file_item?.file_name) : undefined;
  return { kind, platform_handle: handle, ...(name === undefined ? {} : { alt: name }) };
}

function firstNonEmpty(value: string | undefined): string | undefined {
  return typeof value === 'string' && value.length > 0 ? value : undefined;
}
