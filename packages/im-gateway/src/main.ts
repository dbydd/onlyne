import { loadConfig } from './config.js';
import { createImGateway, nodeClock, type ImGateway } from './index.js';

const PUSH_INTERVAL_MS = 5_000;

const path = process.argv[2];
if (path === undefined) {
  console.error('usage: im-gateway <config.json>');
  process.exit(2);
}

const config = await loadConfig(path, process.env);
// The advertised endpoint must carry the port the process actually bound, which is only
// known after `Bun.serve` when the configuration asks for port 0.
let handler: (request: Request) => Promise<Response> = async () =>
  new Response('The gateway is starting.', { status: 503 });
const server = Bun.serve({ hostname: config.server.bind, port: config.server.port, fetch: request => handler(request) });
config.server.port = server.port ?? config.server.port;

const log = (message: string, fields?: Record<string, unknown>): void => {
  console.log(JSON.stringify({ at: new Date().toISOString(), message, ...fields }));
};
const gateway: ImGateway = await createImGateway(config, { log });
handler = request => gateway.node.fetch(request);
gateway.node.client.runMaintenance();

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
  await gateway.stop();
  server.stop(true);
  log('im.gateway.stopped', { node: config.node.id });
  process.exit(0);
};
process.on('SIGINT', () => { void shutdown(); });
process.on('SIGTERM', () => { void shutdown(); });

log('im.gateway.listening', { node: config.node.id, endpoint: `http://${config.server.bind}:${config.server.port}/onlyne/v3`, connectors: config.connectors.map(entry => entry.platform) });
