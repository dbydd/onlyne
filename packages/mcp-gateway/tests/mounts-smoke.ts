import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import {
  OnlyneError, OnlyneNode, type Plugins,
} from '@onlyne/lib';
import { MemoryStorage } from '@onlyne/lib/memory';
import { createMcpGateway, nodeClock, type GatewayConfig, type McpGateway } from '../src/index.js';
import type { MountRun, MountTask } from '../src/mounts.js';
import { FakeMcpServer } from './fake-mcp.js';

/**
 * End-to-end smoke against real sockets: a gateway node and a controller node, both bound on
 * loopback and introduced through discovery, the scheduler's resource API over Onlyne HTTP, the
 * result message read from the controller's own history, a restart on the same store, the status
 * lines the TUI renders, and the CLI's `ONLYNE_NO_TUI=1` JSON stream.
 *
 *   bun tests/mounts-smoke.ts
 *   MOUNT_SMOKE_INTERVAL_MS=3000 bun tests/mounts-smoke.ts   # fast local pass
 */
const say = (message: string, fields: Record<string, unknown> = {}): void => {
  console.log(JSON.stringify({ at: new Date().toISOString(), message, ...fields }));
};

const must = (condition: boolean, detail: string): void => {
  if (!condition) throw new Error(detail);
};

async function waitFor<T>(what: string, load: () => Promise<T>, done: (value: T) => boolean, timeout: number): Promise<T> {
  const deadline = Date.now() + timeout;
  let last: T | undefined;
  while (Date.now() < deadline) {
    last = await load();
    if (done(last)) return last;
    await Bun.sleep(200);
  }
  throw new Error(`timed out waiting for ${what}: ${JSON.stringify(last)}`);
}

const GATEWAY_ID = 'urn:onlyne:gateway:mounts-smoke';
const CONTROLLER_ID = 'urn:onlyne:agent:mounts-controller';
const intervalMs = Number(process.env.MOUNT_SMOKE_INTERVAL_MS ?? 30_000);
const tickMs = Number(process.env.MOUNT_SMOKE_TICK_MS ?? 250);
/** Three runs at a 30s interval, plus a full period of slack for the controller readback. */
const threeRunsBudget = intervalMs * 3 + 45_000;

const dir = await mkdtemp(join(tmpdir(), 'mcp-mounts-smoke-'));
const fake = new FakeMcpServer();
const mutation = () => ({
  idempotency_key: crypto.randomUUID(),
  retry_until: new Date(Date.now() + 300_000).toISOString(),
});

let controllerServer: ReturnType<typeof Bun.serve> | undefined;
let gatewayServer: ReturnType<typeof Bun.serve> | undefined;
let gateway: McpGateway | undefined;
let controller: OnlyneNode | undefined;
let controllerHandler: (request: Request) => Promise<Response> = async () => new Response('starting', { status: 503 });
let gatewayHandler: (request: Request) => Promise<Response> = async () => new Response('starting', { status: 503 });

try {
  // ---- the controller node, serving its real endpoint -------------------------
  controllerServer = Bun.serve({ port: 0, hostname: '127.0.0.1', fetch: request => controllerHandler(request) });
  const controllerBase = `http://127.0.0.1:${controllerServer.port}/onlyne/v3`;
  let node: OnlyneNode;
  const plugins: Plugins = {
    storage: new MemoryStorage(),
    authenticate: async request => {
      const actor = request.headers.get('Onlyne-Actor');
      if (actor === null || actor.length === 0) return null;
      if (actor === CONTROLLER_ID) throw new OnlyneError('forbidden', 'HTTP callers cannot claim the controller identity.');
      return { actor };
    },
    resolve: async id => {
      const target = id === CONTROLLER_ID ? { node_id: id, base_url: controllerBase } : await node.discovery.resolve(id);
      return { ...target, headers: { 'Onlyne-Actor': CONTROLLER_ID } };
    },
    decide: () => { throw new OnlyneError('decision_unavailable', 'The controller decides nothing.'); },
    maintenanceClock: nodeClock(),
  };
  node = new OnlyneNode({
    node_id: CONTROLLER_ID, base_url: controllerBase, plugins,
    authentication: ['none'], now: () => Date.now(), id: () => crypto.randomUUID(),
  });
  controller = node;
  controllerHandler = request => controller!.fetch(request);
  say('controller.listening', { endpoint: controllerBase });

  // ---- the gateway node on the same loopback ----------------------------------
  gatewayServer = Bun.serve({ port: 0, hostname: '127.0.0.1', fetch: request => gatewayHandler(request) });
  const gatewayPort = gatewayServer.port ?? 0;
  must(gatewayPort > 0, 'the gateway bound a port');
  const gatewayBase = `http://127.0.0.1:${gatewayPort}/onlyne/v3`;
  const config: GatewayConfig = {
    node: { id: GATEWAY_ID },
    server: { bind: '127.0.0.1', port: gatewayPort },
    storage_path: join(dir, 'gateway-store.json'),
    servers: [{ name: 'fake', url: fake.url, controllers: [CONTROLLER_ID] }],
    mounts: {
      controllers: [CONTROLLER_ID],
      commands: [
        { id: 'tick', command: process.execPath, args: ['-e', 'console.log("mount tick", new Date().toISOString())'] },
      ],
      tools: [{ server: 'fake', tool: 'echo' }],
      tick_ms: tickMs,
    },
  };
  const openGateway = async (): Promise<McpGateway> => {
    const opened = await createMcpGateway(config);
    gatewayHandler = request => opened.node.fetch(request);
    opened.node.client.runMaintenance();
    return opened;
  };
  gateway = await openGateway();
  await gateway.node.discovery.observe(controllerBase, 'smoke', CONTROLLER_ID);
  await controller.discovery.observe(gatewayBase, 'smoke', GATEWAY_ID);
  controller.client.runMaintenance();
  say('gateway.listening', { endpoint: gatewayBase, interval_ms: intervalMs, tick_ms: tickMs });

  const resource = `mounts:${CONTROLLER_ID}`;
  const operationsPath = `/resources/${encodeURIComponent(GATEWAY_ID)}/${encodeURIComponent(resource)}/operations`;
  const http = async (method: string, path: string, body?: unknown): Promise<{ status: number; body: Record<string, unknown> }> => {
    const response = await fetch(gatewayBase + path, {
      method,
      headers: { 'content-type': 'application/json', 'Onlyne-Actor': CONTROLLER_ID },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    });
    const text = await response.text();
    return { status: response.status, body: (text === '' ? {} : JSON.parse(text)) as Record<string, unknown> };
  };
  const operate = (operation: string, data: Record<string, unknown>): Promise<{ status: number; body: Record<string, unknown> }> =>
    http('POST', operationsPath, { ...mutation(), operation_id: crypto.randomUUID(), operation, data });
  /** The service's own JSON; the smoke narrows at this boundary. */
  const payload = <T>(body: Record<string, unknown>, name: string): T => {
    const data = body.data;
    if (typeof data !== 'object' || data === null || !(name in data)) throw new Error(`the operation returned no ${name}`);
    return (data as Record<string, unknown>)[name] as T;
  };
  const runsOf = async (taskId: string): Promise<MountRun[]> => payload<MountRun[]>((await operate('runs', { task: taskId, limit: 100 })).body, 'runs');

  // ---- mount a 30s interval command through the resource API ------------------
  const mounted = await operate('mount', {
    schedule: { kind: 'interval', every_ms: intervalMs },
    action: { kind: 'command', command: 'tick' },
    title: 'smoke interval',
  });
  must(mounted.status === 201, `mount answered ${mounted.status}`);
  const task = payload<MountTask>(mounted.body, 'task');
  say('mount.created', { task: task.id, next_run_at: task.next_run_at });
  must(task.next_run_at !== null, 'the first interval run is due immediately');

  const finished = await waitFor(
    'three successful runs',
    () => runsOf(task.id),
    runs => runs.filter(run => run.state === 'succeeded').length >= 3,
    threeRunsBudget,
  );
  const three = finished.filter(run => run.state === 'succeeded').slice(0, 3);
  must(three.every(run => run.exit_code === 0), 'every run exited 0');
  must(three.every(run => run.output_summary.includes('mount tick')), 'every run recorded its output');
  must(three.every(run => run.push?.message_id === run.id), 'every run has a stable message id');
  say('runs.three', { runs: three.map(run => ({ id: run.id, at: run.started_at })) });

  // ---- the controller reads the results it was sent ---------------------------
  const inbound = await waitFor(
    'the controller to read three results',
    async () => {
      const page = await controller!.history.list(controller!.auth, { page_size: 50 });
      return page.items.flatMap(conversation => conversation.messages)
        .filter(message => message.direction === 'in' && message.message.delivery_kind === 'mount.run');
    },
    messages => messages.length >= 3,
    40_000,
  ).catch(async error => {
    const diagnostics = await runsOf(task.id);
    const history = await controller!.history.list(controller!.auth, { page_size: 50 });
    say('readback.debug', {
      pushes: diagnostics.map(run => ({ id: run.id, push: run.push })),
      history: history.items.map(conversation => ({ title: conversation.title, messages: conversation.messages.length })),
    });
    throw error;
  });
  const received = inbound.filter(message => message.message.meta?.data !== undefined
    && (message.message.meta.data as { task_id?: string }).task_id === task.id);
  must(received.length >= 3, `the controller holds ${received.length} results for this task`);
  const firstBody = received[0]!.message.msg;
  must(firstBody.includes('mount run') && firstBody.includes('succeeded') && firstBody.includes('mount tick'), `the readable body is ${firstBody.slice(0, 120)}`);
  const structured = received[0]!.message.meta?.data as { run_id?: string; state?: string } | undefined;
  must(structured?.state === 'succeeded', 'the structured result names the run state');
  must(structured?.run_id !== undefined && three.some(run => run.id === structured.run_id), 'the structured result names a real run');
  const accepted = await waitFor('accepted push receipts', () => runsOf(task.id), runs =>
    runs.filter(run => run.push?.state === 'accepted').length >= 3, 20_000);
  must(accepted.filter(run => run.push?.state === 'accepted').length >= 3, 'the queue settled the pushes as accepted');
  say('controller.readback', { results: received.length, seq: received[0]!.seq });

  // While the mount is active its row carries the next trigger, which is what the TUI renders.
  const runningStatus = await gateway.status();
  must(runningStatus.slice(4).some(line => line.includes(task.id) && line.includes('next=')),
    `the active task row is missing from ${JSON.stringify(runningStatus)}`);

  // ---- cancel: no further fires ----------------------------------------------
  const cancelled = await operate('cancel', { task: task.id });
  const cancelledTask = payload<MountTask>(cancelled.body, 'task');
  must(cancelledTask.state === 'cancelled' && cancelledTask.next_run_at === null, 'cancel clears the next trigger');
  const before = (await runsOf(task.id)).length;
  await Bun.sleep(Math.min(intervalMs, 2_500) + 500);
  const after = (await runsOf(task.id)).length;
  must(after === before, `a cancelled task fired again (${before} -> ${after})`);
  say('mount.cancelled', { runs: after });

  // ---- the TUI's data source --------------------------------------------------
  const lines = await gateway.status();
  say('status', { lines });
  must(lines.length <= 8, `status returned ${lines.length} lines`);
  must(lines[0]?.startsWith('servers:') === true && lines[0].includes('fake('), 'status names the servers and tool counts');
  must(lines[1]?.startsWith('mounts:') === true, 'status summarises the mounts');
  must(lines[2]?.startsWith('push:') === true, 'status shows the push queue');
  must(lines[3]?.startsWith('last:') === true, 'status shows the last result');
  must(lines.slice(4).some(line => line.includes('next=')) || !lines[4]?.startsWith('  '), 'status shows a next trigger');

  // ---- restart on the same store ---------------------------------------------
  await gateway.stop();
  gateway = await openGateway();
  const listed = payload<MountTask[]>((await operate('list', {})).body, 'tasks');
  must(listed.some(entry => entry.id === task.id && entry.state === 'cancelled'), 'the task survives the restart');
  const retained = await runsOf(task.id);
  must(retained.length === before, `the run ledger survives the restart (${retained.length} of ${before})`);
  must(retained.every(run => run.state !== 'running'), 'no run is left running after the restart');
  const restartedStatus = await gateway.status();
  must(restartedStatus.some(line => line.startsWith('last:') && line.includes(task.id)),
    `the restarted status lost the retained result: ${JSON.stringify(restartedStatus)}`);
  say('restart', { tasks: listed.length, runs: retained.length });
  await gateway.stop();
  gateway = undefined;

  // ---- the CLI: ONLYNE_NO_TUI=1 keeps the JSON stream -------------------------
  const cliDir = join(dir, 'cli');
  await mkdir(join(cliDir, 'home'), { recursive: true });
  const cliConfig = join(cliDir, 'config.json');
  await writeFile(cliConfig, JSON.stringify({
    node: { id: 'urn:onlyne:gateway:mounts-smoke-cli' },
    server: { bind: '127.0.0.1', port: 0 },
    storage: { path: join(cliDir, 'store.json') },
    servers: [],
  }));
  const cli = Bun.spawn({
    cmd: [process.execPath, join(import.meta.dir, '..', 'src', 'main.ts'), cliConfig],
    env: { ...process.env, ONLYNE_NO_TUI: '1', HOME: join(cliDir, 'home') },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  const decoder = new TextDecoder();
  let stdout = '';
  let stderr = '';
  const drain = async (stream: ReadableStream<Uint8Array>, append: (text: string) => void): Promise<void> => {
    for await (const chunk of stream as unknown as AsyncIterable<Uint8Array>) append(decoder.decode(chunk, { stream: true }));
  };
  const readingOut = drain(cli.stdout as ReadableStream<Uint8Array>, text => { stdout += text; });
  const readingErr = drain(cli.stderr as ReadableStream<Uint8Array>, text => { stderr += text; });
  await waitFor('the CLI to announce its listener', async () => stdout, text => text.includes('"mcp.gateway.listening"'), 25_000);
  cli.kill('SIGTERM');
  const exit = await Promise.race([
    cli.exited.then(code => ({ code })),
    Bun.sleep(10_000).then(() => ({ code: 'timeout' as const })),
  ]);
  await Promise.all([readingOut, readingErr]);
  if (exit.code === 'timeout') cli.kill('SIGKILL');
  must(exit.code === 0, `the CLI exited with ${exit.code}; stderr: ${stderr.slice(0, 400)}`);
  const jsonLines = stdout.split('\n').filter(line => line.trim().length > 0);
  must(jsonLines.length > 0, 'the CLI wrote no log lines');
  for (const line of jsonLines) {
    let parsed: unknown;
    try { parsed = JSON.parse(line); } catch { throw new Error(`a non-JSON line reached stdout: ${line.slice(0, 200)}`); }
    must(typeof parsed === 'object' && parsed !== null, 'every stdout line is a JSON object');
  }
  must(jsonLines.some(line => line.includes('"mcp.gateway.listening"')), 'the listener line is in the JSON stream');
  say('cli.no_tui', { lines: jsonLines.length });

  say('smoke.passed', { interval_ms: intervalMs });
} catch (error) {
  console.error(`mounts smoke failed: ${error instanceof Error ? error.message : String(error)}`);
  process.exitCode = 1;
} finally {
  await gateway?.stop().catch(() => undefined);
  controller?.client.stopMaintenance();
  controllerServer?.stop(true);
  gatewayServer?.stop(true);
  await fake.stop().catch(() => undefined);
  await rm(dir, { recursive: true, force: true });
}
