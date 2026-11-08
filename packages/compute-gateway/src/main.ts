#!/usr/bin/env bun
import { MemoryStorage } from '@onlyne/lib/memory';
import { DockerSandboxDriver, MockSandboxDriver } from '@onlyne/compute-core';
import { GatewayTui, createLogger } from '@onlyne/node-host';
import { existsSync } from 'node:fs';
import { ComputeGatewayNode, ComputeTuiRenderer, nodeClock, type ComputeGatewayConfig } from './index.ts';

const DEFAULT_PORT = 3998;
const DEFAULT_NODE_ID = 'urn:onlyne:gateway:compute';

const config: ComputeGatewayConfig = {
  node: { id: process.env['ONLYNE_NODE_ID'] ?? DEFAULT_NODE_ID },
  server: {
    bind: process.env['ONLYNE_BIND'] ?? '127.0.0.1',
    port: Number(process.env['ONLYNE_PORT'] ?? DEFAULT_PORT),
  },
  storage: {},
};

const storage = new MemoryStorage();
const hasDocker = existsSync('/var/run/docker.sock');
const driver = hasDocker
  ? new DockerSandboxDriver({ id: 'orbstack-docker' })
  : new MockSandboxDriver('default-mock');

let handler: (request: Request) => Promise<Response> = async () =>
  new Response('starting', { status: 503 });
const http = Bun.serve({ hostname: config.server.bind, port: config.server.port, fetch: request => handler(request) });
config.server.port = http.port ?? config.server.port;
const gateway = new ComputeGatewayNode({ config, storage, driver, clock: nodeClock() });
const renderer = new ComputeTuiRenderer(gateway.service);
handler = request => gateway.node.fetch(request);
const endpoint = `http://${config.server.bind}:${config.server.port}/onlyne/v3`;
const interactive = Boolean(process.stdin.isTTY && !process.env['ONLYNE_NO_TUI']);
const logger = createLogger(!interactive, 200);

logger(`onlyne-compute-broker started as ${config.node.id} on ${endpoint} [driver: ${driver.id}]`);

let tui: GatewayTui | undefined;

if (interactive) {
  tui = new GatewayTui(
    gateway.node,
    logger.ring,
    endpoint,
    () => undefined,
    () => renderer.formatStatusLines(),
  );
  await tui.start();
} else {
  console.log(`onlyne-compute-broker listening at ${endpoint} as ${config.node.id} [driver: ${driver.id}]`);
}

const shutdown = async () => {
  if (tui) await tui.close();
  await gateway.close();
  http.stop(true);
  process.exit(0);
};

process.on('SIGINT', shutdown);
process.on('SIGTERM', shutdown);
