#!/usr/bin/env bun
import { createLogger, createNodeKey, startNodeHost, type NodeHostOptions } from '@onlyne/node-host';
import { resolveLocale, setLocale } from '@onlyne/node-host/i18n';
import { dirname, join } from 'node:path';
import { loadConfig } from './config.js';
import { createMcpGateway, type McpGateway } from './index.js';
import { t } from './gateway-text.js';

const path = process.argv[2];
if (path === undefined) {
  console.error(t('cli.usage'));
  process.exit(2);
}

const config = await loadConfig(path);
// The configured language wins over the environment for everything this process prints; the
// logger and the node host below are built after this point so their text follows it too.
setLocale(resolveLocale(config.language));
// On a terminal the TUI owns the screen, so log lines go to the ring it renders and to a file
// beside the store; the ring alone would vanish with the process. `ONLYNE_NO_TUI=1` asks for
// the JSON log even on a terminal, so the operator keeps a machine-readable stream.
const forceJson = process.env.ONLYNE_NO_TUI === '1';
const { log, ring } = createLogger(
  forceJson || !(process.stdin.isTTY && process.stdout.isTTY), 200, join(dirname(config.storage_path), 'gateway.log'),
);
// RFC 0017 §2: the CLI is always keyed, and a missing key is generated and saved under
// ~/.config/onlyne/keys/<node-id>/ instead of being a hand-written prerequisite.
const keys = await createNodeKey(config.node.id);
// The advertised endpoint must carry the port the process actually bound, which is only
// known after `Bun.serve` when the configuration asks for port 0.
let handler: (request: Request) => Promise<Response> = async () =>
  new Response(t('startup.pending'), { status: 503 });
const server = Bun.serve({ hostname: config.server.bind, port: config.server.port, fetch: request => handler(request) });
config.server.port = server.port ?? config.server.port;
const endpoint = `http://${config.server.bind}:${config.server.port}/onlyne/v3`;

const gateway: McpGateway = await createMcpGateway(config, { keys, log });
handler = request => gateway.node.fetch(request);
gateway.node.client.runMaintenance();
// Discovery starts only once the real handler is served: a candidate probe aimed at this
// node must reach the gateway instead of the starting stub.
// The scheduler's status lines ride into the shared trust TUI through the host's status hook;
// the node host owns how they are drawn.
type NodeHostSurface = NodeHostOptions & { status?: () => Promise<string[]> };
const hostOptions: NodeHostSurface = {
  node: gateway.node, endpoint, bind: config.server.bind, ring, log,
  status: () => gateway.status(),
};
const host = await startNodeHost(hostOptions);

const shutdown = async (): Promise<void> => {
  await host.close();
  await gateway.stop();
  server.stop(true);
  log('mcp.gateway.stopped', { node: config.node.id });
  process.exit(0);
};
process.on('SIGINT', () => { void shutdown(); });
process.on('SIGTERM', () => { void shutdown(); });

log('mcp.gateway.listening', {
  node: config.node.id,
  endpoint,
  servers: config.servers.map(entry => entry.name),
});
