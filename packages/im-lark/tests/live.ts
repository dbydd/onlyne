import { readFile } from 'node:fs/promises';
import { Client } from '@larksuiteoapi/node-sdk';
import { createLarkPort } from '../src/index.js';
import type { ImEvent } from '@onlyne/im-core';

/**
 * Live smoke test against a real Feishu app. It is skipped unless a credential is supplied,
 * so `bun run test` stays offline. Usage:
 *
 *   bun tests/live.ts --env ../../archive/examples/im-gateway/.env
 *   bun tests/live.ts --env <path> --send oc_xxx --wait 30000
 *
 * The credential is `app_id:app_secret`; `--send` posts one message to that chat (a visible
 * side effect in the tenant), and `--wait` keeps the socket open to print inbound events.
 */
function argument(name: string): string | undefined {
  const index = process.argv.indexOf(`--${name}`);
  return index < 0 ? undefined : process.argv[index + 1];
}

async function credential(): Promise<string | undefined> {
  const path = argument('env');
  if (path !== undefined) {
    const text = await readFile(path, 'utf8');
    const values: Record<string, string> = {};
    for (const line of text.split('\n')) {
      const match = line.match(/^([A-Z_][A-Z0-9_]*)=(.*)$/);
      if (match?.[1] !== undefined) values[match[1]] = match[2]!.trim();
    }
    if (values.FEISHU_CREDENTIAL !== undefined && values.FEISHU_CREDENTIAL.length > 0) return values.FEISHU_CREDENTIAL;
    const appId = values.FEISHU_APP_ID;
    const appSecret = values.FEISHU_APP_SECRET;
    if (appId !== undefined && appSecret !== undefined) return `${appId}:${appSecret}`;
    return undefined;
  }
  if (process.env.FEISHU_CREDENTIAL) return process.env.FEISHU_CREDENTIAL;
  if (process.env.FEISHU_APP_ID && process.env.FEISHU_APP_SECRET) {
    return `${process.env.FEISHU_APP_ID}:${process.env.FEISHU_APP_SECRET}`;
  }
  return undefined;
}

const value = await credential();
if (value === undefined) {
  console.log('live: no Feishu credential supplied, skipped');
  process.exit(0);
}
const separator = value.indexOf(':');
if (separator <= 0) {
  console.error('live: the credential is app_id:app_secret');
  process.exit(1);
}
const appId = value.slice(0, separator);
const appSecret = value.slice(separator + 1);

const states: string[] = [];
const inbound: ImEvent[] = [];
const port = createLarkPort({
  appId,
  appSecret,
  onState: (state, error) => states.push(error === undefined ? state : `${state}: ${error.message}`),
});

const handlers = {
  message: (raw: unknown) => {
    const message = (raw as { message?: { message_id?: string; chat_id?: string; content?: string } }).message;
    console.log('inbound message:', message?.chat_id, message?.message_id, (message?.content ?? '').slice(0, 80));
  },
  cardAction: () => ({ toast: { type: 'info' as const, content: '已收到' } }),
  reaction: () => { console.log('inbound reaction'); },
  botMember: () => { console.log('inbound bot member change'); },
  recall: () => { console.log('inbound recall'); },
};

console.log('live: connecting with app_id', appId);
const started = Date.now();
await port.start(handlers);
console.log('live: start resolved in', Date.now() - started, 'ms');
console.log('live: bot identity', JSON.stringify(port.botIdentity()));

for (let attempt = 0; attempt < 60; attempt += 1) {
  if (port.status().state === 'connected') break;
  await new Promise(resolve => setTimeout(resolve, 250));
}
console.log('live: connection state', port.status().state);
console.log('live: state callbacks', JSON.stringify(states));

const client = new Client({ appId, appSecret });
try {
  const response = await client.request({ method: 'GET', url: '/open-apis/im/v1/chats?page_size=20' }) as {
    data?: { items?: Array<{ chat_id?: string; name?: string; chat_mode?: string }> };
  };
  const items = response.data?.items ?? [];
  console.log('live: chats visible to the bot:', items.length);
  for (const chat of items) console.log('  -', chat.chat_id, '|', chat.name ?? '(unnamed)', '|', chat.chat_mode ?? '');
} catch (error) {
  console.log('live: chat list failed:', error instanceof Error ? error.message : String(error));
}

const sendTo = argument('send');
if (sendTo !== undefined) {
  try {
    const sent = await port.send(sendTo, { text: `onlyne v3 lark connector smoke ${new Date().toISOString()}` });
    console.log('live: send result', JSON.stringify(sent));
  } catch (error) {
    // A rejection still proves the call reached the platform; print it verbatim.
    console.log('live: send failed', error instanceof Error ? error.message : String(error));
  }
}

const wait = Number(argument('wait') ?? '0');
if (wait > 0) {
  console.log(`live: listening for ${wait} ms; send the bot a message now`);
  await new Promise(resolve => setTimeout(resolve, wait));
}

const historyOf = argument('history');
if (historyOf !== undefined) {
  try {
    const response = await client.im.v1.message.list({
      params: { container_id_type: 'chat', container_id: historyOf, page_size: 5, sort_type: 'ByCreateTimeDesc' },
    });
    const items = response.data?.items ?? [];
    console.log('live: last messages in', historyOf, '→', items.length);
    for (const item of items) {
      console.log(
        '  -', item.message_id, '|', item.msg_type,
        '| parent', item.parent_id ?? '-', '| root', item.root_id ?? '-',
        '|', (item.body?.content ?? '').slice(0, 100),
      );
    }
  } catch (error) {
    console.log('live: history failed:', error instanceof Error ? error.message : String(error));
  }
}

await port.stop();
console.log('live: stopped, final state', port.status().state, '| inbound events seen:', inbound.length);
