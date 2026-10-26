import { mkdir, writeFile } from 'node:fs/promises';
import { dirname } from 'node:path';
import type { ImAttachment, ImEvent } from '@onlyne/im-core';

export interface AttachmentOptions {
  /** Where downloads land; unset keeps every attachment on its platform handle. */
  mediaDir?: string;
  download: (fileKey: string, type: 'image' | 'file') => Promise<Buffer>;
  /** Write hook, injectable so a test records paths without touching the disk. */
  write?: (path: string, data: Buffer) => Promise<void>;
}

/**
 * Downloads inbound attachments under `mediaDir` and backfills `path` (RFC 0012 §9).
 * A download or write failure leaves that attachment on its platform handle and
 * moves on: one unreadable file must not swallow the whole event.
 */
export async function materializeAttachments(event: ImEvent, options: AttachmentOptions): Promise<ImEvent> {
  const { mediaDir } = options;
  if (mediaDir === undefined || event.attachments.length === 0) return event;
  const write = options.write ?? defaultWrite;
  const directory = `${mediaDir}/${sanitize(event.conversation)}`;
  const messageKey = sanitize(event.message_handle ?? event.source_id);
  const attachments: ImAttachment[] = [];
  for (const [index, attachment] of event.attachments.entries()) {
    const data = await options
      .download(attachment.platform_handle, attachment.kind === 'image' ? 'image' : 'file')
      .catch(() => undefined);
    if (data === undefined) {
      attachments.push(attachment);
      continue;
    }
    const path = `${directory}/${messageKey}-${index}-${sanitize(attachment.alt ?? attachment.kind)}`;
    try {
      await write(path, data);
    } catch {
      attachments.push(attachment);
      continue;
    }
    attachments.push({ ...attachment, path });
  }
  return { ...event, attachments };
}

async function defaultWrite(path: string, data: Buffer): Promise<void> {
  await mkdir(dirname(path), { recursive: true });
  await writeFile(path, data);
}

/** One path segment: separators, `..`, and anything outside the safe set become `_`. */
function sanitize(text: string): string {
  return text.replace(/\.\./g, '_').replace(/[^A-Za-z0-9._-]/g, '_');
}
