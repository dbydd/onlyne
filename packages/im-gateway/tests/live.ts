import { readFile, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { MemoryStorage, OnlyneError, OnlyneNode, type Auth, type Json, type Transport } from '@onlyne/lib';
import { createImGateway, nodeClock, type GatewayConfig, type ImGateway } from '../src/index.js';

/**
 * Live acceptance against a real Feishu chat: inbound event → conversation thread projection →
 * agent reply rendered back to the platform → resource handover to a second agent.
 *
 *   bun tests/live.ts --env ../../archive/examples/im-gateway/.env --chat oc_xxx [--wait 180000]
 *
 * The script is skipped without a credential. It prints one JSON line per step; send the bot a
 * message from Feishu when it says it is waiting.
 */
const NODE = 'urn:onlyne:gateway:live';
const AGENT_A = 'urn:onlyne:agent:a';
const AGENT_B = 'urn:onlyne:agent:b';
const RESOURCE = 'chat-live';

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
  console.log('live: needs --env <file with FEISHU_CREDENTIAL> and --chat <chat_id>; skipped');
  process.exit(0);
}
const waitMs = Number(argument('wait') ?? '180000');
const say = (message: string, fields: Record<string, unknown> = {}) => {
  console.log(JSON.stringify({ at: new Date().toISOString(), message, ...fields }));
};

function agentNode(id: string): OnlyneNode {
  return new OnlyneNode({
    node_id: id,
    base_url: `http://${id}.test/onlyne/v3`,
    plugins: {
      storage: new MemoryStorage(),
      authenticate: async request => {
        const actor = request.headers.get('Onlyne-Actor');
        return actor === null || actor.length === 0 ? null : { actor };
      },
      resolve: async () => { throw new OnlyneError('not_found', 'live: this agent resolves nobody.'); },
      decide: () => { throw new OnlyneError('decision_unavailable', 'live: this agent decides nothing.'); },
      maintenanceClock: nodeClock(),
      runtime: {
        notify: () => undefined,
        supports: async () => false,
        inject: async () => { throw new OnlyneError('unsupported_delivery', 'live: no model context.'); },
      },
    },
    now: () => Date.now(),
    id: () => crypto.randomUUID(),
  });
}

const agentA = agentNode(AGENT_A);
const agentB = agentNode(AGENT_B);
const agents = new Map<string, OnlyneNode>([[AGENT_A, agentA], [AGENT_B, agentB]]);
// A node id contains colons (`urn:onlyne:agent:a`), so it cannot be a URL host; the transport
// maps each identity onto its own fixed host instead.
const hosts = new Map<string, string>([[AGENT_A, 'http://agent-a.test'], [AGENT_B, 'http://agent-b.test']]);
const transport: Transport = {
  async request<T>(target: string, method: string, path: string, body?: unknown): Promise<T> {
    const agent = agents.get(target);
    if (agent === undefined) throw new OnlyneError('unavailable', `live: no peer ${target}.`);
    const host = hosts.get(target);
    if (host === undefined) throw new OnlyneError('unavailable', `live: no host for ${target}.`);
    const response = await agent.fetch(new Request(`${host}/onlyne/v3${path}`, {
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

const dir = await mkdtemp(join(tmpdir(), 'im-gateway-live-'));
const config: GatewayConfig = {
  node: { id: NODE },
  server: { bind: '127.0.0.1', port: 39872 },
  storage_path: join(dir, 'store.json'),
  connectors: [{
    platform: 'lark', conversation: chat, assignee: AGENT_A,
    credential: secret,
  }],
};
const gateway: ImGateway = await createImGateway(config, { transport, log: (message, fields) => say(`gateway.${message}`, fields ?? {}) });
const endpoint = 'http://127.0.0.1:39872/onlyne/v3';
const call = async (method: string, path: string, body: unknown, actor: string): Promise<{ status: number; body: Record<string, unknown> }> => {
  const response = await gateway.node.fetch(new Request(endpoint + path, {
    method,
    headers: { 'content-type': 'application/json', 'Onlyne-Actor': actor },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  }));
  const text = await response.text();
  return { status: response.status, body: (text === '' ? {} : JSON.parse(text)) as Record<string, unknown> };
};
const mutation = () => ({ idempotency_key: crypto.randomUUID(), retry_until: new Date(Date.now() + 120_000).toISOString() });
const enc = encodeURIComponent;

/** Reads the resource event rows straight from the gateway's store, in event order. */
const events = async (): Promise<Array<{ event_id: string; source_id: string; recipients: string[]; data: Record<string, Json> }>> => {
  const rows = await gateway.storage.transaction(tx =>
    tx.scan<{ event_id: string; source_id: string; recipients: string[]; data: Record<string, Json> }>(`resource/${enc(NODE)}/${enc(RESOURCE)}/event/`));
  return rows.map(row => row.value).sort((a, b) => Number(a.event_id) - Number(b.event_id));
};

async function waitForEvents(count: number, label: string): Promise<void> {
  const deadline = Date.now() + waitMs;
  say('waiting', { label, want: count });
  while (Date.now() < deadline) {
    if ((await events()).length >= count) return;
    await new Promise(resolve => setTimeout(resolve, 500));
  }
  throw new Error(`live: timed out waiting for ${label}`);
}

try {
  // 1. Inbound: a message from Feishu lands as a resource event and is projected into the thread.
  await waitForEvents(1, 'the first Feishu message');
  const first = (await events())[0]!;
  say('inbound', { event_id: first.event_id, source_id: first.source_id, recipient: first.recipients[0], text: first.data.text });

  const thread = `im-${RESOURCE}`;
  const threadView = await gateway.node.messages.list(gateway.node.auth, { owner: NODE, id: thread }, { attention: 'thread' });
  say('thread', {
    id: thread, entries: threadView.items.length,
    last: threadView.items.at(-1)?.message.msg, seq: threadView.items.at(-1)?.seq,
  });

  // 2. The assignee acknowledges what it has read; the handover fence requires an empty backlog.
  const acked = await call('POST', `/resources/${NODE}/${RESOURCE}/events/${first.event_id}/ack`, mutation(), AGENT_A);
  say('ack', { status: acked.status, acknowledged: (acked.body.acknowledged as string[] | undefined)?.length });

  // 3. The agent answers inside the thread; the renderer sends it to Feishu as a reply.
  const projection = `im-notify-${NODE}-${RESOURCE}-${first.event_id}`;
  const reply = await call('POST', `/threads/${NODE}/${thread}/messages`, {
    ...mutation(),
    message: {
      id: `agent-a-reply-${first.event_id}`, sender: AGENT_A, delivery_kind: 'reply',
      msg: '收到，这里是 agent A 的自动回复。',
      meta: { in_reply_to: { sender: NODE, id: projection } },
    },
    target_endpoint: NODE,
    plan: { response: 'none', execution: 'existing' },
  }, AGENT_A);
  say('reply.admitted', { status: reply.status, seq: reply.body.seq });
  await gateway.maintain();
  const rendered = await gateway.storage.transaction(tx =>
    tx.scan<{ state: string; message_id?: string }>(`im/render/${enc(NODE)}/${enc(RESOURCE)}/seq/`));
  say('render', { records: rendered.map(row => ({ seq: row.key.split('/').at(-1), ...row.value })) });

  // 4. Hand the resource over to agent B: initiate as A, decide as B.
  const handover = await call('POST', `/resources/${NODE}/${RESOURCE}/operations`, {
    ...mutation(), operation_id: 'op-handover-1', operation: 'handover',
    data: { target: AGENT_B, decision_deadline: new Date(Date.now() + 120_000).toISOString() },
  }, AGENT_A);
  say('handover.initiated', { status: handover.status, state: handover.body.state });
  const decided = await call('POST', `/resources/${NODE}/${RESOURCE}/handovers/op-handover-1/decision`, {
    ...mutation(), accepted: true,
  }, AGENT_B);
  say('handover.decided', { status: decided.status, state: decided.body.state });
  const described = await call('GET', `/resources/${NODE}/${RESOURCE}`, undefined, AGENT_A);
  say('resource', { assignee: described.body.assignee, revision: described.body.revision });

  // 5. A second Feishu message must now reach agent B.
  await waitForEvents(2, 'the second Feishu message');
  const second = (await events())[1]!;
  say('inbound.after_handover', {
    event_id: second.event_id, source_id: second.source_id, recipient: second.recipients[0],
    text: second.data.text, routed_to_b: second.recipients[0] === AGENT_B,
  });

  say('done', { note: 'check the Feishu chat: the agent reply should be there, quoting the first message' });
} finally {
  await gateway.stop();
  await rm(dir, { recursive: true, force: true });
}
