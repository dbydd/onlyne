import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { InMemoryTransport } from '@modelcontextprotocol/sdk/inMemory.js';
import { OnlyneError } from '@onlyne/lib';
import { createOnlyneHost, createOnlyneMcpServer, loadConfig, TOOLS } from '../src/index.ts';

let failures = 0;
const ok = (label: string, condition: boolean): void => {
  if (condition) console.log(`ok - ${label}`);
  else { failures++; console.error(`not ok - ${label}`); }
};

const rec = (value: unknown): Record<string, unknown> | undefined =>
  value !== null && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : undefined;

const toolText = (result: unknown): string => {
  const content = rec(result)?.content as Array<{ text?: unknown }> | undefined;
  const text = content?.[0]?.text;
  if (typeof text === 'string') return text;
  return JSON.stringify(text ?? {});
};

const toolBody = (result: unknown): Record<string, unknown> | undefined =>
  rec(JSON.parse(toolText(result)));

const NODE = 'urn:onlyne:agent:mcp';
const dir = await mkdtemp(join(tmpdir(), 'onlyne-mcp-server-'));

const configPath = join(dir, 'mcp-server.json');
await writeFile(configPath, JSON.stringify({
  node: { id: NODE },
  server: { bind: '127.0.0.1', port: 0 },
  storage: { path: join(dir, 'store.json') },
}));

const loaded = await loadConfig(configPath, { ONLYNE_PORT: '3996' });
ok('loadConfig reads the file and ONLYNE_PORT', loaded.node.id === NODE && loaded.server.port === 3996);

try {
  await loadConfig(join(dir, 'missing.json'), {});
  ok('loadConfig refuses an unreadable file', false);
} catch (error) {
  ok('loadConfig refuses an unreadable file', error instanceof OnlyneError);
}

const host = await createOnlyneHost({
  node: { id: NODE },
  server: { bind: '127.0.0.1', port: 0 },
  storage_path: join(dir, 'live.json'),
}, { storage: 'memory' });

const server = createOnlyneMcpServer(host.node);
const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
await server.connect(serverTransport);
const client = new Client({ name: 'onlyne-test', version: '0' });
await client.connect(clientTransport);

const listed = await client.listTools();
const names = listed.tools.map(tool => tool.name);
ok('the server advertises the focused Onlyne tool set',
  TOOLS.every(tool => names.includes(tool.name))
  && names.includes('send_message')
  && names.includes('list_inbox')
  && names.includes('node_status')
  && names.includes('list_neighbors'));

const resources = await client.listResources();
const uris = resources.resources.map(resource => resource.uri);
ok('the server advertises onlyne:// status, inbox, threads and neighbors',
  uris.includes('onlyne://status') && uris.includes('onlyne://inbox')
  && uris.includes('onlyne://threads') && uris.includes('onlyne://neighbors'));

const statusTool = await client.callTool({ name: 'node_status', arguments: {} });
const statusBody = toolBody(statusTool);
ok('node_status reports this node id with empty queues',
  statusTool.isError !== true && statusBody?.node_id === NODE
  && statusBody?.inbox === 0 && statusBody?.threads === 0);

const sent = await client.callTool({
  name: 'send_message',
  arguments: { to: NODE, msg: 'hello from mcp', delivery_kind: 'note' },
});
const sentBody = toolBody(sent);
ok('send_message admits a self-addressed note',
  sent.isError !== true && sentBody?.admission === 'accepted' && rec(sentBody?.receipt)?.seq === '1');

const threads = await client.callTool({ name: 'list_threads', arguments: {} });
const threadPage = toolBody(threads);
const threadItems = Array.isArray(threadPage?.items) ? threadPage.items : [];
const firstThread = rec(threadItems[0]);
const threadRef = rec(firstThread?.thread_ref);
ok('list_threads shows the conversation the send opened',
  threadItems.length >= 1 && threadRef?.owner === NODE);

const messages = await client.callTool({
  name: 'list_messages',
  arguments: { owner: NODE, id: threadRef?.id, attention: 'thread' },
});
const messagePage = toolBody(messages);
const messageItems = Array.isArray(messagePage?.items) ? messagePage.items : [];
const firstMessage = rec(rec(messageItems[0])?.message);
ok('list_messages returns the body this node stored',
  firstMessage?.msg === 'hello from mcp' && firstMessage?.delivery_kind === 'note');

const read = await client.callTool({
  name: 'read_message',
  arguments: { sender: NODE, id: firstMessage?.id },
});
const readBody = toolBody(read);
ok('read_message returns the same stored entry',
  rec(readBody?.message)?.msg === 'hello from mcp');

const inbox = await client.callTool({ name: 'list_inbox', arguments: {} });
ok('list_inbox answers without error', inbox.isError !== true);

const neighbors = await client.callTool({ name: 'list_neighbors', arguments: {} });
ok('list_neighbors answers without error', neighbors.isError !== true);

const unknown = await client.callTool({ name: 'not_a_tool', arguments: {} });
ok('an unknown tool is returned as isError', unknown.isError === true);

const statusResource = await client.readResource({ uri: 'onlyne://status' });
const statusResourceText = statusResource.contents[0] && 'text' in statusResource.contents[0]
  ? String(statusResource.contents[0].text) : '';
ok('onlyne://status reads as JSON with this node id',
  rec(JSON.parse(statusResourceText))?.node_id === NODE);

const missing = await client.callTool({
  name: 'send_message',
  arguments: { msg: 'no target' },
});
ok('send_message without to is an error result', missing.isError === true);

const described = await client.callTool({
  name: 'describe_thread',
  arguments: { owner: NODE, id: threadRef?.id },
});
const describedBody = toolBody(described);
ok('describe_thread returns the owned thread',
  described.isError !== true && rec(rec(describedBody?.thread)?.thread_ref)?.id === threadRef?.id);

const joined = await client.callTool({ name: 'list_joined', arguments: {} });
const joinedBody = toolBody(joined);
ok('list_joined answers with an items page',
  joined.isError !== true && Array.isArray(joinedBody?.items));

await host.node.resources.register(host.node.auth, {
  resource_ref: { owner: NODE, id: 'demo' },
  kind: 'im_conversation',
  operations: ['read', 'send', 'list_events', 'describe', 'handover'],
  binding: { handler: NODE, cursor: '0' },
  log_readers: [NODE],
  event_seq: '0',
  revision: 'rev-1',
  retention_ms: 60_000,
});
const resourcesList = await client.callTool({ name: 'list_resources', arguments: {} });
const resourcesBody = toolBody(resourcesList);
const resourceItems = Array.isArray(resourcesBody?.items) ? resourcesBody.items : [];
ok('list_resources includes the registered resource',
  resourceItems.some(item => rec(rec(item)?.resource_ref)?.id === 'demo'));
const resource = await client.callTool({
  name: 'describe_resource',
  arguments: { owner: NODE, id: 'demo' },
});
ok('describe_resource returns the registered resource',
  resource.isError !== true && rec(toolBody(resource)?.resource_ref)?.id === 'demo');

const mut = () => ({
  idempotency_key: crypto.randomUUID(),
  retry_until: new Date(Date.now() + 60_000).toISOString(),
});
for (const title of ['page-a', 'page-b', 'page-c']) {
  await host.node.threads.create(host.node.auth, { ...mut(), title });
}
const page1 = toolBody(await client.callTool({
  name: 'list_threads', arguments: { page_size: 1 },
}));
const page2 = toolBody(await client.callTool({
  name: 'list_threads', arguments: { page_size: 1, cursor: page1?.next_cursor },
}));
ok('list_threads paginates with page_size and cursor',
  Array.isArray(page1?.items) && page1.items.length === 1
  && typeof page1.next_cursor === 'string' && page1.next_cursor.length > 0
  && Array.isArray(page2?.items) && page2.items.length === 1
  && rec(rec(page1.items[0])?.thread_ref)?.id !== rec(rec(page2.items[0])?.thread_ref)?.id);

const continued = await client.callTool({
  name: 'send_message',
  arguments: {
    to: NODE,
    msg: 'continue',
    delivery_kind: 'reply',
    thread_owner: NODE,
    thread_id: threadRef?.id,
    in_reply_to_sender: NODE,
    in_reply_to_id: firstMessage?.id,
  },
});
const continuedBody = toolBody(continued);
ok('send_message with thread_owner/thread_id and in_reply_to continues the thread',
  continued.isError !== true && continuedBody?.admission === 'accepted'
  && rec(rec(continuedBody?.receipt)?.thread_ref)?.id === threadRef?.id);

const asyncSend = await client.callTool({
  name: 'send_message',
  arguments: { to: NODE, msg: 'async plan', response: 'async' },
});
ok('send_message with response:async is accepted',
  asyncSend.isError !== true && toolBody(asyncSend)?.admission === 'accepted');

const newExec = await client.callTool({
  name: 'send_message',
  arguments: { to: NODE, msg: 'new execution', execution: 'new' },
});
ok('send_message with execution:new is accepted',
  newExec.isError !== true && toolBody(newExec)?.admission === 'accepted');

await client.close();
await server.close();
await host.stop();
await rm(dir, { recursive: true, force: true });

if (failures > 0) {
  console.error(`mcp-server verify: ${failures} check(s) failed`);
  process.exit(1);
}
console.log('mcp-server verify: all checks passed');
