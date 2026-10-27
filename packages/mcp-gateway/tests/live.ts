import { unlink } from 'node:fs/promises';
import { loadConfig } from '../src/config.js';
import { createMcpGateway, type McpGateway } from '../src/index.js';

/**
 * Live acceptance against a real Hindsight bank: the gateway connects, lists tools, and three
 * tool calls run end to end through the Onlyne resource-operation path.
 *
 *   bun tests/live.ts /tmp/onlyne-mcp-live/config.json
 */
const path = process.argv[2];
if (path === undefined) {
  console.error('live: needs a config path');
  process.exit(2);
}
const say = (message: string, fields: Record<string, unknown> = {}): void => {
  console.log(JSON.stringify({ at: new Date().toISOString(), message, ...fields }));
};

const config = await loadConfig(path);
await unlink(config.storage_path).catch(() => undefined);
const gateway: McpGateway = await createMcpGateway(config);
const endpoint = 'http://127.0.0.1:39874/onlyne/v3';
const AGENT = 'urn:onlyne:agent:lazuli';
const mutation = () => ({
  idempotency_key: crypto.randomUUID(),
  retry_until: new Date(Date.now() + 300_000).toISOString(),
});
const call = async (method: string, requestPath: string, body: unknown, actor: string): Promise<{ status: number; body: Record<string, unknown> }> => {
  const response = await gateway.node.fetch(new Request(endpoint + requestPath, {
    method,
    headers: { 'content-type': 'application/json', 'Onlyne-Actor': actor },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  }));
  const text = await response.text();
  return { status: response.status, body: (text === '' ? {} : JSON.parse(text)) as Record<string, unknown> };
};

try {
  // 1. The resource registers with the real tool list.
  const described = await call('GET', '/resources/urn%3Aonlyne%3Agateway%3Amcp/hindsight', undefined, AGENT);
  const operations = described.body.operations as string[] | undefined;
  say('resource', {
    status: described.status,
    operations: operations?.length,
    has_retain: operations?.includes('sync_retain'),
    has_recall: operations?.includes('recall'),
  });

  // 2. A write tool: retain, then read it back.
  const retained = await call('POST', '/resources/urn%3Aonlyne%3Agateway%3Amcp/hindsight/operations', {
    ...mutation(), operation_id: 'op-retain-1', operation: 'sync_retain',
    data: { content: 'the mcp gateway live test stores this line', tags: ['onlyne', 'mcp-live'] },
  }, AGENT);
  const memoryIds = ((retained.body.data as { structuredContent?: { memory_ids?: string[] } } | undefined)?.structuredContent?.memory_ids) ?? [];
  say('sync_retain', { state: retained.body.state, memory_ids: memoryIds });

  // 3. A read tool: recall finds it.
  const recalled = await call('POST', '/resources/urn%3Aonlyne%3Agateway%3Amcp/hindsight/operations', {
    ...mutation(), operation_id: 'op-recall-1', operation: 'recall',
    data: { query: 'mcp gateway live test', budget: 'low' },
  }, AGENT);
  const results = ((recalled.body.data as { structuredContent?: { results?: unknown[] } } | undefined)?.structuredContent?.results) ?? [];
  say('recall', { state: recalled.body.state, results: results.length });

  // 4. An isError path: a read-only tool on a missing id.
  const failed = await call('POST', '/resources/urn%3Aonlyne%3Agateway%3Amcp/hindsight/operations', {
    ...mutation(), operation_id: 'op-get-1', operation: 'get_memory',
    data: { memory_id: '00000000-0000-0000-0000-000000000000' },
  }, AGENT);
  say('get_memory.miss', {
    status: failed.status, state: failed.body.state,
    source: (failed.body.error as { details?: { source?: string } } | undefined)?.details?.source?.slice(0, 80),
  });

  // 5. A tool outside the list never leaves the node.
  const refused = await call('POST', '/resources/urn%3Aonlyne%3Agateway%3Amcp/hindsight/operations', {
    ...mutation(), operation_id: 'op-ref-1', operation: 'not_a_tool', data: {},
  }, AGENT);
  say('unknown_tool', { status: refused.status, code: (refused.body.error as { code?: string } | undefined)?.code });

  say('done', {});
} finally {
  await gateway.stop();
}
