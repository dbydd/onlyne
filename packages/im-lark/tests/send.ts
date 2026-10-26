import type { SendInput, SendOptions, SendResult } from '@larksuiteoapi/node-sdk';
import type { ImEvent, SendRequest } from '@onlyne/im-core';
import { materializeAttachments } from '../src/attachments.js';
import { sendMessage, type SendPort } from '../src/send.js';

const checks: Array<[string, boolean]> = [];
function check(name: string, condition: boolean): void {
  checks.push([name, condition]);
}

function same(a: unknown, b: unknown): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

interface Call {
  to: string;
  input: SendInput;
  opts?: SendOptions;
}

/** A port that records every call and answers from `script`, defaulting to a fresh message id. */
function recordingPort(script: Array<SendResult | Error> = []): { port: SendPort; calls: Call[] } {
  const calls: Call[] = [];
  const port: SendPort = {
    async send(to, input, opts) {
      calls.push({ to, input, opts });
      const next = script[calls.length - 1];
      if (next instanceof Error) throw next;
      return next ?? { messageId: `om_${calls.length}` };
    },
  };
  return { port, calls };
}

function request(overrides: Partial<SendRequest> = {}): SendRequest {
  return { conversation: 'oc_chat', text: 'hello', format: 'plain', attachments: [], ...overrides };
}

// --- head shape ---
const plain = recordingPort();
const plainOutcome = await sendMessage(plain.port, request());
check('plain text head is {text}', same(plain.calls[0]?.input, { text: 'hello' }));
check('a plain send is one message', plain.calls.length === 1);
check('a request without reply fields passes no opts', plain.calls[0]?.opts === undefined);
check('the send targets the conversation coordinate', plain.calls[0]?.to === 'oc_chat');
check('a plain send commits with the head message_id', plainOutcome.state === 'committed' && plainOutcome.message_id === 'om_1');
check('a clean send carries no notes', plainOutcome.notes === undefined);

const markdown = recordingPort();
await sendMessage(markdown.port, request({ text: '**hi**', format: 'markdown' }));
check('markdown head is {markdown}', same(markdown.calls[0]?.input, { markdown: '**hi**' }));

const card = { schema: '2.0', body: {} };
const cardPort = recordingPort();
await sendMessage(cardPort.port, request({ text: 'ignored', format: 'markdown', card }));
check('a card wins over text and markdown', same(cardPort.calls[0]?.input, { card }));
check('a card send is one message', cardPort.calls.length === 1);

// --- reply passthrough ---
const reply = recordingPort();
await sendMessage(reply.port, request({ reply_to: 'om_parent' }));
check('reply_to becomes replyTo', same(reply.calls[0]?.opts, { replyTo: 'om_parent' }));

const threaded = recordingPort();
await sendMessage(threaded.port, request({ reply_in_thread: true }));
check('reply_in_thread becomes replyInThread', same(threaded.calls[0]?.opts, { replyInThread: true }));

const both = recordingPort();
await sendMessage(both.port, request({ reply_to: 'om_parent', reply_in_thread: false }));
check('both reply fields travel together, false included', same(both.calls[0]?.opts, { replyTo: 'om_parent', replyInThread: false }));

// --- attachments as separate segments ---
const segments = recordingPort();
const segmentsOutcome = await sendMessage(
  segments.port,
  request({
    reply_to: 'om_parent',
    attachments: [
      { kind: 'image', path: '/tmp/a.png' },
      { kind: 'file', path: '/tmp/report.pdf' },
      { kind: 'audio', path: '/tmp/voice.opus' },
      { kind: 'video', path: '/tmp/clip.mp4' },
      { kind: 'sticker', path: '/tmp/sticker.webp' },
    ],
  }),
);
check('head plus four uploadable attachments is five sends', segments.calls.length === 5);
check('an image segment is {image:{source}}', same(segments.calls[1]?.input, { image: { source: '/tmp/a.png' } }));
check('a file segment carries the basename', same(segments.calls[2]?.input, { file: { source: '/tmp/report.pdf', fileName: 'report.pdf' } }));
check('an audio segment is {audio:{source}}', same(segments.calls[3]?.input, { audio: { source: '/tmp/voice.opus' } }));
check('a video segment is {video:{source}}', same(segments.calls[4]?.input, { video: { source: '/tmp/clip.mp4' } }));
check(
  'the reply coordinate rides on the head only',
  same(segments.calls[0]?.opts, { replyTo: 'om_parent' }) && segments.calls.slice(1).every(call => call.opts === undefined),
);
check('the head message_id is the operation result', segmentsOutcome.state === 'committed' && segmentsOutcome.message_id === 'om_1');
check('the skipped sticker is recorded by request index', same(segmentsOutcome.notes, ['attachment 4: sticker cannot be uploaded, skipped']));

// --- an empty body promotes the first attachment ---
const promoted = recordingPort();
const promotedOutcome = await sendMessage(
  promoted.port,
  request({
    text: '',
    reply_to: 'om_parent',
    attachments: [
      { kind: 'image', path: '/tmp/one.png' },
      { kind: 'image', path: '/tmp/two.png' },
    ],
  }),
);
check('an empty body makes the first attachment the head', same(promoted.calls[0]?.input, { image: { source: '/tmp/one.png' } }));
check('the promoted head carries the reply coordinate', same(promoted.calls[0]?.opts, { replyTo: 'om_parent' }));
check('the promoted head is not sent twice', promoted.calls.length === 2);
check('the second attachment carries no reply', promoted.calls[1]?.opts === undefined);
check('the promoted head message_id is the result', promotedOutcome.message_id === 'om_1');

const nothing = recordingPort();
const nothingOutcome = await sendMessage(nothing.port, request({ text: '', attachments: [{ kind: 'sticker', path: '/tmp/s.webp' }] }));
check('a request with nothing sendable never reaches the port', nothing.calls.length === 0);
check('a request with nothing sendable is unknown', nothingOutcome.state === 'unknown' && nothingOutcome.error !== undefined);

// --- failure states ---
const headFailure = recordingPort([new Error('lark: invalid receive_id')]);
const headFailureOutcome = await sendMessage(
  headFailure.port,
  request({ attachments: [{ kind: 'image', path: '/tmp/a.png' }] }),
);
check('a head failure is unknown', headFailureOutcome.state === 'unknown');
check('a head failure carries the platform text', headFailureOutcome.error === 'lark: invalid receive_id');
check('a head failure stops the send', headFailure.calls.length === 1);
check('a head failure reports no message_id', headFailureOutcome.message_id === undefined);

const attachmentFailure = recordingPort([{ messageId: 'om_head' }, new Error('upload rejected')]);
const attachmentFailureOutcome = await sendMessage(
  attachmentFailure.port,
  request({ attachments: [{ kind: 'image', path: '/tmp/a.png' }, { kind: 'image', path: '/tmp/b.png' }] }),
);
check(
  'an attachment failure keeps the send committed',
  attachmentFailureOutcome.state === 'committed' && attachmentFailureOutcome.message_id === 'om_head',
);
check('an attachment failure is recorded by request index', same(attachmentFailureOutcome.notes, ['attachment 0: upload rejected']));
check('a failed attachment does not stop the next one', attachmentFailure.calls.length === 3);

// --- attachment materialization ---
const event: ImEvent = {
  platform: 'lark',
  source_id: 'evt_1',
  conversation: 'oc_chat',
  kind: 'message',
  text: 'see attached',
  attachments: [
    { kind: 'image', platform_handle: 'img_key', alt: 'screenshot' },
    { kind: 'file', platform_handle: 'file_key', alt: '../secret' },
  ],
  message_handle: 'om_1',
  at: '1730000000000',
};

const downloaded: Array<{ fileKey: string; type: string }> = [];
const written: Array<{ path: string; size: number }> = [];
const materialized = await materializeAttachments(event, {
  mediaDir: '/work/media',
  download: async (fileKey, type) => {
    downloaded.push({ fileKey, type });
    return Buffer.from(fileKey);
  },
  write: async (path, data) => {
    written.push({ path, size: data.byteLength });
  },
});
check('an image downloads as type=image', same(downloaded[0], { fileKey: 'img_key', type: 'image' }));
check('a file downloads as type=file', same(downloaded[1], { fileKey: 'file_key', type: 'file' }));
check('the image path is mediaDir/conversation/message-index-alt', written[0]?.path === '/work/media/oc_chat/om_1-0-screenshot');
check('separators and traversal in alt are sanitized', written[1]?.path === '/work/media/oc_chat/om_1-1-__secret');
check('the downloaded bytes reach the write hook', written[0]?.size === Buffer.from('img_key').byteLength);
check(
  'the local path is backfilled and the platform handle kept',
  materialized.attachments[0]?.path === '/work/media/oc_chat/om_1-0-screenshot'
    && materialized.attachments[0]?.platform_handle === 'img_key',
);
check('the event body survives materialization', materialized.text === 'see attached' && materialized.attachments.length === 2);

let untouchedDownloads = 0;
const untouched = await materializeAttachments(event, {
  download: async () => {
    untouchedDownloads += 1;
    return Buffer.alloc(0);
  },
});
check('no mediaDir returns the event untouched', untouched === event && untouchedDownloads === 0);

const downloadFailure = await materializeAttachments(event, {
  mediaDir: '/work/media',
  download: async fileKey => {
    if (fileKey === 'img_key') throw new Error('410 gone');
    return Buffer.from('ok');
  },
  write: async () => {},
});
check(
  'a failed download keeps the platform handle and no path',
  downloadFailure.attachments[0]?.platform_handle === 'img_key' && downloadFailure.attachments[0]?.path === undefined,
);
check('a failed download does not stop the next attachment', downloadFailure.attachments[1]?.path !== undefined);

const writeFailure = await materializeAttachments(event, {
  mediaDir: '/work/media',
  download: async () => Buffer.from('x'),
  write: async () => {
    throw new Error('EACCES');
  },
});
check(
  'a failed write leaves no path on any attachment',
  writeFailure.attachments.length === 2 && writeFailure.attachments.every(attachment => attachment.path === undefined),
);

for (const [name, pass] of checks) console.log(`${pass ? 'ok' : 'FAIL'}  ${name}`);
const failed = checks.filter(([, pass]) => !pass).length;
console.log(`\n${checks.length - failed}/${checks.length} assertions passed`);
if (failed > 0) process.exit(1);
