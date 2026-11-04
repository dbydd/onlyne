/**
 * Process-level stdio smoke: spawn onlyne-mcp-server and speak MCP over stdin/stdout.
 */
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { getDefaultEnvironment, StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';

let failures = 0;
const ok = (label: string, condition: boolean): void => {
  if (condition) console.log(`ok - ${label}`);
  else { failures++; console.error(`not ok - ${label}`); }
};

const dir = await mkdtemp(join(tmpdir(), 'mcp-stdio-'));
const configPath = join(dir, 'mcp-server.json');
await writeFile(configPath, JSON.stringify({
  node: { id: 'urn:onlyne:agent:mcp-stdio-smoke' },
  server: { bind: '127.0.0.1', port: 0 },
  storage: { path: join(dir, 'store.json') },
}));

const env: Record<string, string> = { ...getDefaultEnvironment() };
for (const [name, value] of Object.entries(process.env)) {
  if (value !== undefined && env[name] === undefined) env[name] = value;
}
env.HOME = dir;
env.ONLYNE_NO_TUI = '1';

const transport = new StdioClientTransport({
  command: process.execPath,
  args: [join(import.meta.dir, '../src/main.ts'), configPath],
  env,
  cwd: join(import.meta.dir, '..'),
  stderr: 'pipe',
});
const client = new Client({ name: 'onlyne-stdio-smoke', version: '0' });

try {
  await client.connect(transport);
  const listed = await client.listTools();
  const names = listed.tools.map(tool => tool.name);
  ok('stdio process advertises send_message and list_inbox',
    names.includes('send_message') && names.includes('list_inbox') && names.includes('node_status'));
  const status = await client.callTool({ name: 'node_status', arguments: {} });
  ok('stdio process answers node_status', status.isError !== true);
} catch (error) {
  failures += 1;
  console.error(`not ok - stdio process smoke: ${error instanceof Error ? error.message : String(error)}`);
} finally {
  try { await client.close(); } catch { /* already closed */ }
  await rm(dir, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`mcp-server stdio: ${failures} check(s) failed`);
  process.exit(1);
}
console.log('mcp-server stdio: all checks passed');
