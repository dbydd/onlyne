import { deepStrictEqual } from 'node:assert/strict';
import type { SendRequest } from '@onlyne/im-core';
import { createSendState, sendMessage, type QqMessagePort } from '../src/send.js';
import { QqPortError, type QqMessageBody, type QqSendResult, type QqSendTarget } from '../src/port.js';

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

interface Recorded {
  target: QqSendTarget;
  body: QqMessageBody;
}

/** A port that records every call and answers from `answers`, defaulting to a fresh message id. */
function recordingPort(answers: Array<QqSendResult | Error> = []): { port: QqMessagePort; calls: Recorded[] } {
  const calls: Recorded[] = [];
  const port: QqMessagePort = {
    async send(target, body) {
      calls.push({ target, body });
      const next = answers[calls.length - 1];
      if (next instanceof Error) throw next;
      return next ?? { id: `msg_${calls.length}` };
    },
  };
  return { port, calls };
}

function request(overrides: Partial<SendRequest> = {}): SendRequest {
  return { conversation: 'c2c:openid_user', text: 'hello', format: 'plain', attachments: [], ...overrides };
}

const plain = recordingPort();
const plainOutcome = await sendMessage(plain.port, request(), createSendState());
same('plain text: msg_type 0 with content', plain.calls[0], {
  target: { kind: 'c2c', openid: 'openid_user' },
  body: { msg_type: 0, content: 'hello' },
});
same('plain text: the response id becomes the message id', plainOutcome, { state: 'committed', message_id: 'msg_1' });

const markdown = recordingPort([{ id: 'msg_md', ext_info: { ref_idx: 'REFIDX_out' } }]);
const markdownOutcome = await sendMessage(markdown.port, request({ format: 'markdown', text: '**bold**' }), createSendState());
same('markdown: msg_type 2 with markdown and no content', markdown.calls[0]?.body, { msg_type: 2, markdown: { content: '**bold**' } });
same('markdown: ref_idx becomes the ref handle', markdownOutcome, { state: 'committed', message_id: 'msg_md', ref_handle: 'REFIDX_out' });

const group = recordingPort();
await sendMessage(group.port, request({ conversation: 'group:group_1' }), createSendState());
same('group conversation addresses the group', group.calls[0]?.target, { kind: 'group', openid: 'group_1' });

const passive = recordingPort();
const state = createSendState();
await sendMessage(passive.port, request({ reply_to: 'msg_in|REFIDX_parent' }), state);
same('passive reply with a quote reference carries both ids', passive.calls[0]?.body, {
  msg_type: 0, content: 'hello', msg_id: 'msg_in', msg_seq: 1, message_reference: { message_id: 'REFIDX_parent' },
});
await sendMessage(passive.port, request({ reply_to: 'msg_in|REFIDX_parent', text: 'second' }), state);
same('a repeated passive id advances msg_seq', [passive.calls[1]?.body.msg_id, passive.calls[1]?.body.msg_seq], ['msg_in', 2]);

const passiveOnly = recordingPort();
await sendMessage(passiveOnly.port, request({ reply_to: 'msg_in' }), createSendState());
same('a bare passive id sends no message_reference', passiveOnly.calls[0]?.body, {
  msg_type: 0, content: 'hello', msg_id: 'msg_in', msg_seq: 1,
});
ok('a bare passive id never invents a quote', passiveOnly.calls[0]?.body.message_reference === undefined);

const attachmentPort = recordingPort();
const attachmentOutcome = await sendMessage(
  attachmentPort.port,
  request({ attachments: [{ kind: 'image', path: '/tmp/a.png' }] }),
  createSendState(),
);
same('an attachment is refused locally, without a platform call', [attachmentOutcome, attachmentPort.calls.length], [
  { state: 'committed', error: 'unsupported by @onlyne/im-qq: the connector sends text and markdown only, so attachments/cards were not sent.' },
  0,
]);

const cardPort = recordingPort();
const cardOutcome = await sendMessage(cardPort.port, request({ card: { type: 'ark' } }), createSendState());
same('a card is refused locally, without a platform call', [cardOutcome.state, cardPort.calls.length], ['committed', 0]);

const badTarget = recordingPort();
const badTargetOutcome = await sendMessage(badTarget.port, request({ conversation: 'user:openid_user' }), createSendState());
same('an unknown conversation scope is refused', [badTargetOutcome.state, badTarget.calls.length], ['unknown', 0]);
ok('the refusal names the accepted coordinates', /c2c:<openid> or group:<openid>/.test(badTargetOutcome.error ?? ''));

const emptyPort = recordingPort();
const emptyOutcome = await sendMessage(emptyPort.port, request({ text: '   ' }), createSendState());
same('an empty body sends nothing', [emptyOutcome.state, emptyPort.calls.length], ['unknown', 0]);

const rejectedPort = recordingPort([new QqPortError('The platform rejected POST /v2/users/x/messages: 22000 msg_id invalid', { status: 400, code: 22000, detail: 'msg_id invalid' })]);
const rejectedOutcome = await sendMessage(rejectedPort.port, request({ reply_to: 'msg_in' }), createSendState());
same('a platform refusal is settled with the platform text', rejectedOutcome, {
  state: 'committed', error: 'The platform rejected the send: 22000 msg_id invalid',
});

const failedPort = recordingPort([new QqPortError('The QQ request POST /v2/users/x/messages did not complete: socket hang up', { status: 0 })]);
const failedOutcome = await sendMessage(failedPort.port, request(), createSendState());
same('a transport failure is unknown', failedOutcome.state, 'unknown');
ok('a transport failure keeps the transport text', /socket hang up/.test(failedOutcome.error ?? ''));

const serverPort = recordingPort([new QqPortError('The QQ platform failed POST /v2/users/x/messages with HTTP 502: bad gateway', { status: 502, detail: 'bad gateway' })]);
const serverOutcome = await sendMessage(serverPort.port, request(), createSendState());
same('a 5xx answer is unknown, not a rejection', [serverOutcome.state, serverOutcome.error], [
  'unknown', 'The QQ platform failed POST /v2/users/x/messages with HTTP 502: bad gateway',
]);

if (failures > 0) {
  console.error(`${failures} check(s) failed`);
  process.exit(1);
}
console.log('send: all checks passed');
