/**
 * The QQ event envelope, translated into one `ImEvent`. Pure: no clock beyond the injected one,
 * no network, no state — the same payload always normalizes the same way.
 *
 * Envelope: `{id, op, d, s, t}`. `op 0` is a dispatch and `t` names the event; anything else
 * (heartbeat, callback validation) is not an inbound event.
 */

import type { ImAttachment, ImEvent, ImReply } from '@onlyne/im-core';
import type { Json } from '@onlyne/lib';

const PLATFORM = 'qq';

const MESSAGE_EVENTS: Record<string, true> = {
  C2C_MESSAGE_CREATE: true,
  GROUP_AT_MESSAGE_CREATE: true,
};
const LIFECYCLE_EVENTS: Record<string, true> = {
  GROUP_ADD_ROBOT: true,
  GROUP_DEL_ROBOT: true,
  FRIEND_ADD: true,
  FRIEND_DEL: true,
  C2C_MSG_RECEIVE: true,
  C2C_MSG_REJECT: true,
  GROUP_MSG_RECEIVE: true,
  GROUP_MSG_REJECT: true,
};

type Raw = Record<string, unknown>;

function recordOf(value: unknown): Raw | undefined {
  return value !== null && typeof value === 'object' && !Array.isArray(value) ? value as Raw : undefined;
}

function textOf(value: unknown): string | undefined {
  return typeof value === 'string' && value.length > 0 ? value : undefined;
}

function senderOf(d: Raw): string | undefined {
  const author = recordOf(d.author);
  return textOf(author?.user_openid) ?? textOf(author?.id)
    ?? textOf(d.openid) ?? textOf(d.op_member_openid) ?? textOf(d.user_openid);
}

/**
 * `group:<group_openid>` when the event names a group, else `c2c:<openid>`. A c2c message keeps
 * its openid under `author`, not at the top level.
 */
function conversationOf(d: Raw): string | undefined {
  const group = textOf(d.group_openid);
  if (group !== undefined) return `group:${group}`;
  const user = senderOf(d);
  return user === undefined ? undefined : `c2c:${user}`;
}

/** `message_scene.ext` carries `msg_idx=REFIDX_…` for this message and `ref_msg_idx=…` for the quote. */
function sceneIndexes(d: Raw): { self?: string; quoted?: string } {
  const ext = recordOf(d.message_scene)?.ext;
  const indexes: { self?: string; quoted?: string } = {};
  if (!Array.isArray(ext)) return indexes;
  for (const entry of ext) {
    if (typeof entry !== 'string') continue;
    const separator = entry.indexOf('=');
    if (separator <= 0) continue;
    const name = entry.slice(0, separator);
    const value = entry.slice(separator + 1);
    if (value.length === 0) continue;
    if (name === 'msg_idx') indexes.self = value;
    else if (name === 'ref_msg_idx') indexes.quoted = value;
  }
  return indexes;
}

function attachmentsOf(d: Raw): ImAttachment[] {
  if (!Array.isArray(d.attachments)) return [];
  const attachments: ImAttachment[] = [];
  for (const entry of d.attachments) {
    const item = recordOf(entry);
    if (item === undefined) continue;
    const contentType = (textOf(item.content_type) ?? '').toLowerCase();
    const kind: ImAttachment['kind'] = contentType.startsWith('image/')
      ? 'image'
      : contentType === 'voice' || contentType.startsWith('audio/')
        ? 'audio'
        : contentType.startsWith('video/')
          ? 'video'
          : 'file';
    const url = textOf(item.url);
    // A voice attachment arrives as SILK; the WAV url is the one a player can open.
    const handle = kind === 'audio' ? textOf(item.voice_wav_url) ?? url : url ?? textOf(item.voice_wav_url);
    if (handle === undefined) continue;
    const filename = textOf(item.filename);
    attachments.push({ kind, platform_handle: handle, ...(filename === undefined ? {} : { alt: filename }) });
  }
  return attachments;
}

/** The first voice attachment's server-side transcript, when the platform produced one. */
function transcriptOf(d: Raw): string | undefined {
  if (!Array.isArray(d.attachments)) return undefined;
  for (const entry of d.attachments) {
    const item = recordOf(entry);
    if (item === undefined) continue;
    const contentType = (textOf(item.content_type) ?? '').toLowerCase();
    if (contentType !== 'voice' && !contentType.startsWith('audio/')) continue;
    const transcript = textOf(item.asr_refer_text);
    if (transcript !== undefined) return transcript;
  }
  return undefined;
}

/** `msg_elements[0].content` of a quote reply: the platform inlines the quoted body there. */
function quotedTextOf(d: Raw): string | undefined {
  if (!Array.isArray(d.msg_elements)) return undefined;
  const first = recordOf(d.msg_elements[0]);
  return textOf(first?.content);
}

function messageEvent(d: Raw, envelope: Raw, now: () => number): ImEvent | undefined {
  const conversation = conversationOf(d);
  const messageId = textOf(d.id);
  if (conversation === undefined || messageId === undefined) return undefined;
  const messageType = typeof d.message_type === 'number' ? d.message_type : undefined;
  const indexes = sceneIndexes(d);
  const quotedText = messageType === 103 ? quotedTextOf(d) : undefined;
  const attachments = attachmentsOf(d);
  const content = textOf(d.content);
  const arkPrompt = textOf(recordOf(d.ark_data)?.prompt);

  const text = content
    ?? (quotedText ?? arkPrompt)
    ?? transcriptOf(d)
    ?? '';

  const replyParts: ImReply = {};
  if (indexes.quoted !== undefined) replyParts.parent = indexes.quoted;
  if (quotedText !== undefined) replyParts.quoted_text = quotedText;
  const sender = senderOf(d);

  return {
    platform: PLATFORM,
    source_id: messageId,
    conversation,
    kind: 'message',
    ...(sender === undefined ? {} : { from: sender }),
    text,
    attachments,
    ...(Object.keys(replyParts).length === 0 ? {} : { reply: replyParts }),
    message_handle: messageId,
    // The passive reply id and the quote index are two ids for one message; the bridge hands the
    // composite back as `reply_to` and `send` splits it again.
    ref_handle: indexes.self === undefined ? messageId : `${messageId}|${indexes.self}`,
    at: textOf(d.timestamp) ?? new Date(now()).toISOString(),
    raw: envelope as unknown as Json,
  };
}

function interactionEvent(d: Raw, envelope: Raw, now: () => number): ImEvent | undefined {
  const id = textOf(d.id);
  const conversation = conversationOf(d);
  if (id === undefined || conversation === undefined) return undefined;
  const sender = senderOf(d);
  return {
    platform: PLATFORM,
    source_id: id,
    conversation,
    kind: 'action',
    ...(sender === undefined ? {} : { from: sender }),
    text: '',
    attachments: [],
    action: { id, ...(d.data === undefined ? {} : { value: d.data as Json }) },
    at: textOf(d.timestamp) ?? new Date(now()).toISOString(),
    raw: envelope as unknown as Json,
  };
}

function lifecycleEvent(type: string, d: Raw, envelope: Raw, now: () => number): ImEvent | undefined {
  const sourceId = textOf(envelope.id) ?? textOf(d.id);
  const conversation = conversationOf(d);
  if (sourceId === undefined || conversation === undefined) return undefined;
  const sender = senderOf(d);
  return {
    platform: PLATFORM,
    source_id: sourceId,
    conversation,
    kind: 'event',
    ...(sender === undefined ? {} : { from: sender }),
    text: '',
    attachments: [],
    event: { type },
    at: textOf(d.timestamp) ?? new Date(now()).toISOString(),
    raw: envelope as unknown as Json,
  };
}

/** One `op 0` dispatch; `undefined` for an event this connector does not translate. */
export function normalizeEvent(payload: unknown, now: () => number = Date.now): ImEvent | undefined {
  const envelope = recordOf(payload);
  if (envelope === undefined || envelope.op !== 0) return undefined;
  const type = textOf(envelope.t);
  const d = recordOf(envelope.d);
  if (type === undefined || d === undefined) return undefined;
  if (MESSAGE_EVENTS[type] === true) return messageEvent(d, envelope, now);
  if (type === 'INTERACTION_CREATE') return interactionEvent(d, envelope, now);
  if (LIFECYCLE_EVENTS[type] === true) return lifecycleEvent(type, d, envelope, now);
  return undefined;
}
