#!/usr/bin/env bun
import { createLogger, createNodeKey, startNodeHost, type NodeHostOptions } from '@onlyne/node-host';
import { resolveLocale, setLocale } from '@onlyne/node-host/i18n';
import { dirname, join } from 'node:path';
import { loadConfig } from './config.ts';
import { createOnlyneHost } from './node.ts';
import { connectStdio, createOnlyneMcpServer } from './server.ts';
import { t } from './server-text.ts';

const path = process.argv[2];
if (path === undefined) {
  console.error(t('cli.usage'));
  process.exit(2);
}

const config = await loadConfig(path, process.env);
setLocale(resolveLocale(config.language));
// Stdio is the MCP transport, so the JSON log cannot use stdout. The file beside the store
// keeps a durable record; stderr is the operator-visible stream.
const logger = createLogger(false, 200, join(dirname(config.storage_path), 'mcp-server.log'));
const log = (message: string, fields?: Record<string, unknown>): void => {
  logger.log(message, fields);
  console.error(JSON.stringify({ at: new Date().toISOString(), level: 'info', message, ...fields }));
};

let handler: (request: Request) => Promise<Response> = async () =>
  new Response(t('startup.pending'), { status: 503 });
const http = Bun.serve({ hostname: config.server.bind, port: config.server.port, fetch: request => handler(request) });
config.server.port = http.port ?? config.server.port;

const keys = await createNodeKey(config.node.id);
const hostNode = await createOnlyneHost(config, { keys, log });
handler = request => hostNode.node.fetch(request);
hostNode.node.client.runMaintenance();
const endpoint = `http://${config.server.bind}:${config.server.port}/onlyne/v3`;
const hostOptions: NodeHostOptions = {
  node: hostNode.node, endpoint, bind: config.server.bind, log, interactive: false,
  ...(config.peers === undefined ? {} : { seeds: config.peers }),
};
const host = await startNodeHost(hostOptions);
const mcp = createOnlyneMcpServer(hostNode.node);
await connectStdio(mcp);

const shutdown = async (): Promise<void> => {
  hostNode.node.client.stopMaintenance();
  await mcp.close();
  await host.close();
  await hostNode.stop();
  http.stop(true);
  log('onlyne.mcp.stopped', { node: config.node.id });
  process.exit(0);
};
process.on('SIGINT', () => { void shutdown(); });
process.on('SIGTERM', () => { void shutdown(); });

log('onlyne.mcp.listening', { node: config.node.id, endpoint, transport: 'stdio' });
