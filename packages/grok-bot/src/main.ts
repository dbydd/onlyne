#!/usr/bin/env bun
import { createLogger, createNodeKey, startNodeHost, type NodeHostOptions } from '@onlyne/node-host';
import { resolveLocale, setLocale } from '@onlyne/node-host/i18n';
import { dirname, join } from 'node:path';
import { loadConfig, redactWebhook } from './config.ts';
import { createGrokBotNode, nodeClock, SERVE_IDLE_TIMEOUT_SEC, type GrokBotNode } from './index.ts';
import { t } from './node-text.ts';

const PUSH_INTERVAL_MS = 2_000;

const path = process.argv[2];
if (path === undefined) {
  console.error(t('cli.usage'));
  process.exit(2);
}

const config = await loadConfig(path, process.env);
setLocale(resolveLocale(config.language));
const toStdout = process.env.ONLYNE_NO_TUI === '1' || !(process.stdin.isTTY && process.stdout.isTTY);
const logger = createLogger(toStdout, 200, join(dirname(config.storage_path), 'gateway.log'));
const { log, ring } = logger;

let handler: (request: Request) => Promise<Response> = async () =>
  new Response(t('startup.pending'), { status: 503 });
const server = Bun.serve({
  hostname: config.server.bind,
  port: config.server.port,
  idleTimeout: SERVE_IDLE_TIMEOUT_SEC,
  fetch: request => handler(request),
});
config.server.port = server.port ?? config.server.port;

const keys = await createNodeKey(config.node.id);
const grok: GrokBotNode = await createGrokBotNode(config, { keys, log: logger });
handler = request => grok.fetch(request);
grok.node.client.runMaintenance();
const endpoint = `http://${config.server.bind}:${config.server.port}/onlyne/v3`;
const hostOptions: NodeHostOptions & { status?: () => Promise<string[]> } = {
  node: grok.node,
  endpoint,
  bind: config.server.bind,
  ...(config.peers === undefined ? {} : { seeds: config.peers }),
  ring,
  log,
  status: () => grok.statusLines(),
};
const host = await startNodeHost(hostOptions);
await grok.approvePinned();

const clock = nodeClock();
let timer: unknown;
let stopping = false;
const tick = async (): Promise<void> => {
  if (stopping) return;
  try { await grok.maintain(); } catch (error) { logger.warn('grok.maintain.failed', { error: String(error) }); }
  if (!stopping) timer = clock.setTimer(PUSH_INTERVAL_MS, () => { void tick(); });
};
timer = clock.setTimer(PUSH_INTERVAL_MS, () => { void tick(); });

const shutdown = async (): Promise<void> => {
  if (stopping) return;
  stopping = true;
  clock.clearTimer(timer);
  grok.node.client.stopMaintenance();
  await host.close();
  await grok.stop();
  server.stop(true);
  log('grok.bot.stopped', { node: config.node.id });
  process.exit(0);
};
process.on('SIGINT', () => { void shutdown(); });
process.on('SIGTERM', () => { void shutdown(); });

log('grok.bot.listening', {
  node: config.node.id,
  endpoint,
  reply: config.reply_path,
  events: config.events_path,
  idleTimeout: SERVE_IDLE_TIMEOUT_SEC,
  pins: config.trust_seeds?.length ?? 0,
  webhook: redactWebhook(config.webhook).url,
});
