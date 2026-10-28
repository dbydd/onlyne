/**
 * One outbound message, translated into a QQ v2 message body. QQ carries no attachments and no
 * cards through this connector, so a request that has them is settled locally instead of being
 * half-sent.
 */

import type { SendOutcome, SendRequest } from '@onlyne/im-core';
import { QqPortError, type QqMessageBody, type QqSendResult, type QqSendTarget } from './port.js';

const UNSUPPORTED = 'unsupported by @onlyne/im-qq: the connector sends text and markdown only, so attachments/cards were not sent.';

/** How many passive ids keep their `msg_seq`; beyond this the oldest is dropped. */
const SEQUENCE_LIMIT = 1024;

/** The port slice `send` needs; `QqPort` satisfies it as it stands. */
export interface QqMessagePort {
  send(target: QqSendTarget, body: QqMessageBody): Promise<QqSendResult>;
}

/**
 * `msg_seq` is per passive message id: QQ rejects a second message with the same `msg_id` and
 * `msg_seq`, so the connector counts how many answers it already sent into one event.
 */
export interface SendState {
  readonly sequences: Map<string, number>;
}

export function createSendState(): SendState {
  return { sequences: new Map() };
}

export function parseConversation(conversation: string): QqSendTarget | undefined {
  const separator = conversation.indexOf(':');
  if (separator <= 0) return undefined;
  const scope = conversation.slice(0, separator);
  const openid = conversation.slice(separator + 1);
  if (openid.length === 0) return undefined;
  if (scope === 'c2c') return { kind: 'c2c', openid };
  if (scope === 'group') return { kind: 'group', openid };
  return undefined;
}

/** The platform's answer as the bridge needs it: a refusal is settled, an unclear transport is not. */
export function outcomeOf(error: unknown, subject: 'send' | 'operation'): SendOutcome {
  if (error instanceof QqPortError) {
    // QQ answers some refusals with HTTP 200 and a non-zero `code`; either form is settled.
    const refused = (error.status >= 400 && error.status < 500)
      || (error.status >= 200 && error.status < 300 && error.code !== undefined);
    if (refused) {
      const label = [error.code === undefined ? '' : String(error.code), error.detail ?? '']
        .filter(part => part.length > 0)
        .join(' ');
      return { state: 'committed', error: `The platform rejected the ${subject}: ${label.length > 0 ? label : error.message}` };
    }
    return { state: 'unknown', error: error.message };
  }
  return { state: 'unknown', error: error instanceof Error ? error.message : String(error) };
}

export async function sendMessage(port: QqMessagePort, request: SendRequest, state: SendState): Promise<SendOutcome> {
  if (request.attachments.length > 0 || request.card !== undefined) {
    return { state: 'committed', error: UNSUPPORTED };
  }
  const target = parseConversation(request.conversation);
  if (target === undefined) {
    return {
      state: 'unknown',
      error: `The QQ connector addresses c2c:<openid> or group:<openid>, not ${request.conversation}.`,
    };
  }
  if (request.text.trim().length === 0) {
    return { state: 'unknown', error: 'nothing to send: the QQ connector sends text or markdown only.' };
  }
  try {
    const result = await port.send(target, messageBody(request, state));
    const id = typeof result.id === 'string' && result.id.length > 0 ? result.id : undefined;
    const ref = result.ext_info?.ref_idx;
    return {
      state: 'committed',
      ...(id === undefined ? {} : { message_id: id }),
      ...(typeof ref === 'string' && ref.length > 0 ? { ref_handle: ref } : {}),
    };
  } catch (error) {
    return outcomeOf(error, 'send');
  }
}

function messageBody(request: SendRequest, state: SendState): QqMessageBody {
  // msg_type 2 carries `markdown` and no `content`: QQ renders whichever field matches the type.
  const body: QqMessageBody = request.format === 'markdown'
    ? { msg_type: 2, markdown: { content: request.text } }
    : { msg_type: 0, content: request.text };
  return { ...body, ...passiveReply(request.reply_to, state) };
}

/**
 * `reply_to` is `msg_id|REFIDX`: the passive reply id, and the quote reference when the inbound
 * event carried one. A single part answers passively without quoting anything.
 */
function passiveReply(replyTo: string | undefined, state: SendState): Partial<QqMessageBody> {
  if (replyTo === undefined || replyTo.length === 0) return {};
  const separator = replyTo.indexOf('|');
  const messageId = separator < 0 ? replyTo : replyTo.slice(0, separator);
  const quote = separator < 0 ? '' : replyTo.slice(separator + 1);
  if (messageId.length === 0) return {};
  const sequence = (state.sequences.get(messageId) ?? 0) + 1;
  state.sequences.set(messageId, sequence);
  if (state.sequences.size > SEQUENCE_LIMIT) {
    for (const key of state.sequences.keys()) {
      if (key === messageId) continue;
      state.sequences.delete(key);
      break;
    }
  }
  return {
    msg_id: messageId,
    msg_seq: sequence,
    ...(quote.length === 0 ? {} : { message_reference: { message_id: quote } }),
  };
}

