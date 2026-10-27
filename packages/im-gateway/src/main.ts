import { createLogger, createNodeKey, startNodeHost } from '@onlyne/node-host';
import { resolveLocale, setLocale } from '@onlyne/node-host/i18n';
import { dirname, join } from 'node:path';
import { loadConfig } from './config.js';
import { createImGateway, nodeClock, type ImGateway } from './index.js';
import { t } from './gateway-text.js';

const PUSH_INTERVAL_MS = 5_000;

const path = process.argv[2];
if (path === undefined) {
  console.error(t('cli.usage'));
  process.exit(2);
}

const config = await loadConfig(path, process.env);
// The configured language wins over the environment for everything this process prints; the
// logger and the node host below are built after this point so their text follows it too.
setLocale(resolveLocale(config.language));
// The advertised endpoint must carry the port the process actually bound, which is only
// known after `Bun.serve` when the configuration asks for port 0.
let handler: (request: Request) => Promise<Response> = async () =>
  new Response(t('startup.pending'), { status: 503 });
const server = Bun.serve({ hostname: config.server.bind, port: config.server.port, fetch: request => handler(request) });
config.server.port = server.port ?? config.server.port;

// On a terminal the TUI owns the screen, so log lines go to the ring it renders and to a file
// beside the store; the ring alone would vanish with the process.
const { log, ring } = createLogger(
  !(process.stdin.isTTY && process.stdout.isTTY), 200, join(dirname(config.storage_path), 'gateway.log'),
);
// RFC 0017 §2: the identity key is load-or-create, so a restart keeps the fingerprint peers
// already confirmed. A gateway that cannot hold a key does not run.
const keys = await createNodeKey(config.node.id);
const gateway: ImGateway = await createImGateway(config, { keys, log });
handler = request => gateway.fetch(request);
gateway.node.client.runMaintenance();
const endpoint = `http://${config.server.bind}:${config.server.port}/onlyne/v3`;
const host = await startNodeHost({ node: gateway.node, endpoint, bind: config.server.bind, seeds: config.peers, ring, log });

const clock = nodeClock();
let timer: unknown;
let stopping = false;
const tick = async (): Promise<void> => {
  if (stopping) return;
  try { await gateway.maintain(); } catch (error) { log('im.maintain.failed', { error: String(error) }); }
  if (!stopping) timer = clock.setTimer(PUSH_INTERVAL_MS, () => { void tick(); });
};
timer = clock.setTimer(PUSH_INTERVAL_MS, () => { void tick(); });

const shutdown = async (): Promise<void> => {
  if (stopping) return;
  stopping = true;
  clock.clearTimer(timer);
  gateway.node.client.stopMaintenance();
  await host.close();
  await gateway.stop();
  server.stop(true);
  log('im.gateway.stopped', { node: config.node.id });
  process.exit(0);
};
process.on('SIGINT', () => { void shutdown(); });
process.on('SIGTERM', () => { void shutdown(); });

log('im.gateway.listening', { node: config.node.id, endpoint, connectors: config.connectors.map(entry => entry.platform) });
