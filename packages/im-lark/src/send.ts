import { basename } from 'node:path';
import type { SendInput, SendOptions, SendResult } from '@larksuiteoapi/node-sdk';
import type { SendOutcome, SendRequest } from '@onlyne/im-core';

/**
 * The slice of the Lark SDK channel the connector sends through. `LarkChannel`
 * satisfies it as it stands; the connector builds one without connecting it
 * (docs/design/im/README.md §5.1).
 */
export interface SendPort {
  send(to: string, input: SendInput, opts?: SendOptions): Promise<SendResult>;
}

/**
 * One `send` operation as a head message plus one message per attachment. The
 * reply coordinate rides on the head only: a reply chain that pointed at the
 * last uploaded image was v2's first defect (README §5.3).
 */
export async function sendMessage(port: SendPort, request: SendRequest): Promise<SendOutcome> {
  const notes: string[] = [];
  const segments: Array<{ index: number; input: SendInput }> = [];
  for (const [index, attachment] of request.attachments.entries()) {
    const input = attachmentInput(attachment);
    if (input === undefined) {
      notes.push(`attachment ${index}: ${attachment.kind} cannot be uploaded, skipped`);
      continue;
    }
    segments.push({ index, input });
  }

  let head = headInput(request);
  if (head === undefined) head = segments.shift()?.input;
  if (head === undefined) {
    return withNotes(
      { state: 'unknown', error: 'nothing to send: no text, card, or uploadable attachment' },
      notes,
    );
  }

  let headResult: SendResult;
  try {
    headResult = await port.send(request.conversation, head, replyOptions(request));
  } catch (error) {
    return withNotes({ state: 'unknown', error: detailOf(error) }, notes);
  }

  for (const segment of segments) {
    try {
      await port.send(request.conversation, segment.input);
    } catch (error) {
      notes.push(`attachment ${segment.index}: ${detailOf(error)}`);
    }
  }
  return withNotes({ state: 'committed', message_id: headResult.messageId }, notes);
}

/** Card first, then markdown, then text; an empty body leaves no head. */
function headInput(request: SendRequest): SendInput | undefined {
  if (request.card !== undefined) return { card: request.card as object };
  if (request.text.trim().length === 0) return undefined;
  return request.format === 'markdown' ? { markdown: request.text } : { text: request.text };
}

function attachmentInput(attachment: SendRequest['attachments'][number]): SendInput | undefined {
  const source = attachment.path;
  switch (attachment.kind) {
    case 'image':
      return { image: { source } };
    case 'file':
      return { file: { source, fileName: basename(source) } };
    case 'audio':
      return { audio: { source } };
    case 'video':
      return { video: { source } };
    case 'sticker':
      // Feishu uploads no stickers: a sticker id is inbound-only, so this one cannot round-trip.
      return undefined;
  }
}

function replyOptions(request: SendRequest): SendOptions | undefined {
  const opts: SendOptions = {};
  if (request.reply_to !== undefined) opts.replyTo = request.reply_to;
  if (request.reply_in_thread !== undefined) opts.replyInThread = request.reply_in_thread;
  return Object.keys(opts).length === 0 ? undefined : opts;
}

function withNotes(outcome: SendOutcome, notes: string[]): SendOutcome {
  return notes.length === 0 ? outcome : { ...outcome, notes };
}

function detailOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
