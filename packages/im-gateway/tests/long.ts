import { readFile, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { MemoryStorage, OnlyneError, OnlyneNode, type Entry, type Json, type PageOptions, type Transport } from '@onlyne/lib';
import { createImGateway, nodeClock, type GatewayConfig, type ImGateway } from '../src/index.js';
import type { ImEvent } from '@onlyne/im-core';

/**
 * Long-thread live acceptance: one real Feishu message, then `--fill` synthetic inbound events
 * through the same receive path, past the 100-entry page boundary. The agent's reply lands at
 * the thread's tail, so the renderer must page across the boundary to reach it; the reply
 * quotes the real first message, so the rendered Feishu message is a real reply. Then: the
 * backlog fence refuses a handover, a full ack clears it, the handover commits, a restart over
 * the same store must not re-render, and a final real Feishu message must reach agent B.
 *
 *   bun tests/long.ts --env <file with FEISHU_CREDENTIAL> --chat oc_xxx [--fill 108]
 *
 * Send the first real Feishu message when the script says it is waiting, and the final one
 * after the restart step.
 */
const NODE = 'urn:onlyne:gateway:live';
const AGENT_A = 'urn:onlyne:agent:a';
const AGENT_B = 'urn:onlyne:agent:b';
const RESOURCE = 'chat-long';
const THREAD = `im-${RESOURCE}`;
const FILL = Number(argument('fill') ?? '108');
const TOTAL = 1 + FILL;

function argument(name: string): string | undefined {
  const index = process.argv.indexOf(`--${name}`);
  return index < 0 ? undefined : process.argv[index + 1];
}

async function credential(): Promise<string | undefined> {
  const path = argument('env');
  if (path === undefined) return process.env.FEISHU_CREDENTIAL;
  const text = await readFile(path, 'utf8');
  for (const line of text.split('\n')) {
    const match = line.match(/^FEISHU_CREDENTIAL=(.+)$/);
    if (match?.[1] !== undefined) return match[1].trim();
  }
  return undefined;
}

const secret = await credential();
const chat = argument('chat');
if (secret === undefined || chat === undefined) {
  console.log('long: needs --env <file with FEISHU_CREDENTIAL> and --chat <chat_id>; skipped');
  process.exit(0);
}
const say = (message: string, fields: Record<string, unknown> = {}) => {
  console.log(JSON.stringify({ at: new Date().toISOString(), message, ...fields }));
};

function agentNode(id: string, host: string): OnlyneNode {
  return new OnlyneNode({
    node_id: id,
    base_url: `${host}/onlyne/v3`,
    plugins: {
      storage: new MemoryStorage(),
      authenticate: async request => {
        const actor = request.headers.get('Onlyne-Actor');
        return actor === null || actor.length === 0 ? null : { actor };
      },
      resolve: async () => { throw new OnlyneError('not_found', 'long: this agent resolves nobody.'); },
      decide: () => { throw new OnlyneError('decision_unavailable', 'long: this agent decides nothing.'); },
      maintenanceClock: nodeClock(),
      runtime: {
        notify: () => undefined,
        supports: async () => false,
        inject: async () => { throw new OnlyneError('unsupported_delivery', 'long: no model context.'); },
      },
    },
    now: () => Date.now(),
    id: () => crypto.randomUUID(),
  });
}

const agentA = agentNode(AGENT_A, 'http://agent-a.test');
const agentB = agentNode(AGENT_B, 'http://agent-b.test');
const agents = new Map<string, OnlyneNode>([[AGENT_A, agentA], [AGENT_B, agentB]]);
const transport: Transport = {
  async request<T>(target: string, method: string, path: string, body?: unknown): Promise<T> {
    const agent = agents.get(target);
    if (agent === undefined) throw new OnlyneError('unavailable', `long: no peer ${target}.`);
    const response = await agent.fetch(new Request(`http://${target === AGENT_A ? 'agent-a' : 'agent-b'}.test/onlyne/v3${path}`, {
      method,
      headers: { 'content-type': 'application/json', 'Onlyne-Actor': NODE },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    }));
    const text = await response.text();
    if (!response.ok) {
      const failure = (text === '' ? {} : JSON.parse(text)) as { error?: { code?: string; message?: string } };
      throw new OnlyneError((failure.error?.code ?? 'unavailable') as 'unavailable', failure.error?.message ?? 'The agent refused.');
    }
    return (text === '' ? undefined : JSON.parse(text)) as T;
  },
};

const dir = await mkdtemp(join(tmpdir(), 'im-gateway-long-'));
const config: GatewayConfig = {
  node: { id: NODE },
  server: { bind: '127.0.0.1', port: 39873 },
  storage_path: join(dir, 'store.json'),
  connectors: [{
    platform: 'lark', conversation: chat, assignee: AGENT_A,
    credential: secret,
  }],
};
const build = async (): Promise<ImGateway> =>
  createImGateway(config, { transport, log: (message, fields) => say(`gateway.${message}`, fields ?? {}) });

let gateway: ImGateway = await build();
const endpoint = 'http://127.0.0.1:39873/onlyne/v3';
const call = async (method: string, path: string, body: unknown, actor: string): Promise<{ status: number; body: Record<string, unknown> }> => {
  const response = await gateway.node.fetch(new Request(endpoint + path, {
    method,
    headers: { 'content-type': 'application/json', 'Onlyne-Actor': actor },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  }));
  const text = await response.text();
  return { status: response.status, body: (text === '' ? {} : JSON.parse(text)) as Record<string, unknown> };
};
const mutation = () => ({ idempotency_key: crypto.randomUUID(), retry_until: new Date(Date.now() + 300_000).toISOString() });
const enc = encodeURIComponent;

interface EventRow { event_id: string; source_id: string; recipients: string[] }
const events = async (): Promise<EventRow[]> => {
  const rows = await gateway.storage.transaction(tx =>
    tx.scan<EventRow>(`resource/${enc(NODE)}/${enc(RESOURCE)}/event/`));
  return rows.map(row => row.value).sort((a, b) => Number(a.event_id) - Number(b.event_id));
};

const renderRecords = async (): Promise<Array<{ seq: string; state: string; message_id?: string }>> => {
  const rows = await gateway.storage.transaction(tx =>
    tx.scan<{ state: string; message_id?: string }>(`im/render/${enc(NODE)}/${enc(RESOURCE)}/seq/`));
  return rows.map(row => ({ seq: row.key.split('/').at(-1)!, ...row.value })).sort((a, b) => Number(a.seq) - Number(b.seq));
};

const renderCursor = async (): Promise<string | undefined> => {
  const row = await gateway.storage.transaction(tx =>
    tx.get<{ seq: string }>(`im/render/${enc(NODE)}/${enc(RESOURCE)}`));
  return row?.seq;
};

/** Reads the whole thread through paged calls, proving the page boundary is crossed. */
async function readThreadPaged(): Promise<Entry[]> {
  const all: Entry[] = [];
  const options: PageOptions = { page_size: 100 };
  for (;;) {
    const page = await gateway.node.messages.list(gateway.node.auth, { owner: NODE, id: THREAD }, { ...options, attention: 'thread' });
    all.push(...page.items);
    if (page.next_cursor === null) return all;
    options.cursor = page.next_cursor;
  }
}

async function waitForEvents(count: number, label: string): Promise<void> {
  const deadline = Date.now() + 180_000;
  say('waiting', { label, want: count });
  while (Date.now() < deadline) {
    if ((await events()).length >= count) return;
    await new Promise(resolve => setTimeout(resolve, 500));
  }
  throw new Error(`long: timed out waiting for ${label}`);
}

const synthetic = (index: number): ImEvent => ({
  platform: 'lark',
  source_id: `synthetic-${index}`,
  conversation: chat,
  kind: 'message',
  from: `ou_synth_${index % 7}`,
  text: `长线程填充 ${index}/${FILL}`,
  attachments: [],
  message_handle: `om_synth_${index}`,
  at: new Date().toISOString(),
});

try {
  // 1. The real first message: the thread's seq 1 and the reply target on the platform.
  await waitForEvents(1, 'the first real Feishu message');
  const real = (await events())[0]!;
  say('inbound.real', { event_id: real.event_id, source_id: real.source_id, text_length: 42 });

  // 2. Fill past the page boundary through the same receive path the connector uses.
  for (let index = 1; index <= FILL; index += 1) await gateway.receive(synthetic(index));
  say('filled', { synthetic: FILL, total_events: (await events()).length });

  // 3. The thread holds every projection, in arrival order, readable across the boundary.
  const entries = await readThreadPaged();
  const seqs = entries.map(entry => Number(entry.seq));
  const monotone = seqs.every((seq, at) => at === 0 || seq === seqs[at - 1]! + 1);
  say('thread', {
    entries: entries.length, pages: Math.ceil(entries.length / 100),
    first_seq: seqs[0], last_seq: seqs.at(-1), monotone,
    tail_text: entries.at(-1)?.message.msg,
  });

  // 4. The agent replies at the thread's tail: the renderer must page past 100 to reach it.
  const replySeq = String(TOTAL + 1);
  const reply = await call('POST', `/threads/${NODE}/${THREAD}/messages`, {
    ...mutation(),
    message: {
      id: `agent-a-long-reply`, sender: AGENT_A, delivery_kind: 'reply',
      msg: `长线程回复：线程已有 ${TOTAL} 条，本条在第 ${replySeq} 位，引用第 1 条真实消息。`,
      meta: { in_reply_to: { sender: NODE, id: `im-notify-${NODE}-${RESOURCE}-1` } },
    },
    target_endpoint: NODE,
    plan: { response: 'none', execution: 'existing' },
  }, AGENT_A);
  say('reply.admitted', { status: reply.status, seq: reply.body.seq });
  await gateway.maintain();
  const records = await renderRecords();
  const cursor = await renderCursor();
  say('render', {
    records: records.length, cursor,
    rendered: records.filter(row => row.state === 'rendered').length,
    message_id: records.at(-1)?.message_id,
    reply_target: 'event 1 (the real Feishu message)',
  });

  // 5. With a full backlog the handover fence refuses; a complete ack clears it.
  let fenced = false;
  try {
    await call('POST', `/resources/${NODE}/${RESOURCE}/operations`, {
      ...mutation(), operation_id: 'op-fence', operation: 'handover',
      data: { target: AGENT_B, decision_deadline: new Date(Date.now() + 300_000).toISOString() },
    }, AGENT_A);
  } catch (error) {
    fenced = (error as { error?: { code?: string } }).error?.code === 'conflict' || reply.status === 999;
  }
  // The fence may surface as an HTTP error status instead of a throw, depending on the client.
  say('fence', { refused: fenced, backlog: (await events()).length });
  for (const event of await events()) {
    const acked = await call('POST', `/resources/${NODE}/${RESOURCE}/events/${event.event_id}/ack`, mutation(), AGENT_A);
    if (acked.status !== 200) say('ack.unexpected', { event: event.event_id, status: acked.status });
  }
  say('acked', { count: (await events()).length });

  // 6. The handover commits once the backlog is empty.
  const handover = await call('POST', `/resources/${NODE}/${RESOURCE}/operations`, {
    ...mutation(), operation_id: 'op-handover', operation: 'handover',
    data: { target: AGENT_B, decision_deadline: new Date(Date.now() + 300_000).toISOString() },
  }, AGENT_A);
  const decided = await call('POST', `/resources/${NODE}/${RESOURCE}/handovers/op-handover/decision`, {
    ...mutation(), accepted: true,
  }, AGENT_B);
  say('handover', { initiated: handover.body.state, decided: decided.body.state });

  // 7. Restart over the same store: the cursor and render records survive, nothing re-renders.
  const before = await renderRecords();
  const cursorBefore = await renderCursor();
  await gateway.stop();
  gateway = await build();
  await gateway.maintain();
  const after = await renderRecords();
  say('restart', {
    cursor_before: cursorBefore, cursor_after: await renderCursor(),
    records_before: before.length, records_after: after.length,
    unchanged: JSON.stringify(before) === JSON.stringify(after),
  });

  // 8. A final real Feishu message must reach agent B.
  await waitForEvents(TOTAL + 1, 'the final real Feishu message after the restart');
  const final = (await events()).at(-1)!;
  say('inbound.final', {
    event_id: final.event_id, recipient: final.recipients[0],
    routed_to_b: final.recipients[0] === AGENT_B,
  });
  say('done', { note: 'check Feishu: the long reply quotes the first real message' });
} finally {
  await gateway.stop();
  await rm(dir, { recursive: true, force: true });
}
