import type { SendOutcome, SendRequest } from '@onlyne/im-core';
import { WechatPortError, detailOf, type WechatOutboundMessage } from './port.js';

/** The one platform call a send makes; the connector's port satisfies it. */
export interface SendPort {
  sendMessage(message: WechatOutboundMessage): Promise<unknown>;
}

const UNSUPPORTED = 'unsupported by @onlyne/im-wechat';

/** The conversation coordinate iLink addresses: `user:<id>`, or `group:<id>` whose id goes to `to_user_id`. */
export function conversationTarget(conversation: string): { scheme: 'user' | 'group'; id: string } | undefined {
  if (conversation.startsWith('user:')) {
    const id = conversation.slice('user:'.length);
    return id.length === 0 ? undefined : { scheme: 'user', id };
  }
  if (conversation.startsWith('group:')) {
    const id = conversation.slice('group:'.length);
    return id.length === 0 ? undefined : { scheme: 'group', id };
  }
  return undefined;
}

/**
 * One text send. `reply_to` starting with `ctx:` is the inbound event's transport context, used
 * as `context_token`. iLink exposes no message-quoting field, so any other handle is not echoed;
 * the text still goes out and the platform decides.
 */
export async function sendMessage(port: SendPort, request: SendRequest): Promise<SendOutcome> {
  if (request.attachments.length > 0 || request.card !== undefined) {
    return {
      state: 'committed',
      error: `${UNSUPPORTED}: the iLink Bot API path implemented here sends text only; attachments and cards are not sent.`,
    };
  }
  const target = conversationTarget(request.conversation);
  if (target === undefined) {
    return { state: 'committed', error: `${UNSUPPORTED}: conversation ${request.conversation} is not user:<id> or group:<id>.` };
  }
  const replyTo = request.reply_to;
  // `ctx:` is the inbound event's transport context. Any other handle is the event's message id,
  // which the iLink Bot API has no field to quote; the text is still sent and the platform's own
  // answer decides, instead of this connector refusing a reply the bridge legitimately addressed.
  const contextToken = replyTo !== undefined && replyTo.startsWith('ctx:') ? replyTo.slice('ctx:'.length) : undefined;
  const clientId = request.request_id ?? crypto.randomUUID();

  try {
    await port.sendMessage({
      from_user_id: '',
      to_user_id: target.id,
      client_id: clientId,
      message_type: 2,
      message_state: 2,
      ...(contextToken === undefined || contextToken.length === 0 ? {} : { context_token: contextToken }),
      item_list: [{ type: 1, text_item: { text: request.text } }],
    });
    return { state: 'committed', message_id: clientId };
  } catch (failure) {
    return outcomeOf(failure);
  }
}

/**
 * A platform answer (`ret`/`errmsg` present) settles the operation: the platform saw the call and
 * refused it. Anything else — no answer, an HTTP failure, a rate limit — leaves it unknown,
 * because the send may still have landed.
 */
export function outcomeOf(failure: unknown): SendOutcome {
  if (failure instanceof WechatPortError && failure.ret !== undefined) {
    return { state: 'committed', error: failure.message };
  }
  return { state: 'unknown', error: detailOf(failure) };
}
