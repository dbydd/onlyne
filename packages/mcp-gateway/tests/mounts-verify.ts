import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createMcpGateway, type GatewayConfig, type McpGateway } from '../src/index.js';
import { nextCronTime, parseCron } from '../src/cron.js';
import type { MountRun, MountTask } from '../src/mounts.js';
import { FakeMcpServer } from './fake-mcp.js';

let failures = 0;

const expect = (condition: boolean, detail: string): void => {
  if (!condition) throw new Error(detail);
};

/** One test step: a thrown expectation is this label's failure, never a crashed suite. */
async function step(label: string, run: () => Promise<void>): Promise<void> {
  try {
    await run();
    console.log(`ok - ${label}`);
  } catch (error) {
    failures++;
    console.error(`not ok - ${label}: ${error instanceof Error ? error.message : String(error)}`);
  }
}

const cronNext = (expression: string, from: string): string | undefined => {
  const next = nextCronTime(parseCron(expression), Date.parse(from));
  return next === undefined ? undefined : new Date(next).toISOString();
};

const NODE = 'urn:onlyne:gateway:mcp';
const CONTROLLER = 'urn:onlyne:agent:mount-worker';
const INTRUDER = 'urn:onlyne:agent:intruder';
const RESOURCE_ID = `mounts:${CONTROLLER}`;
const resourcePath = (id: string): string => `/resources/${encodeURIComponent(NODE)}/${encodeURIComponent(id)}`;
const mutation = () => ({
  idempotency_key: crypto.randomUUID(),
  retry_until: new Date(Date.now() + 60_000).toISOString(),
});

/** Operation responses are the service's own JSON; the test narrows at this boundary. */
function field<T>(body: Record<string, unknown>, name: string): T {
  const data = body.data;
  if (typeof data !== 'object' || data === null || !(name in data)) {
    throw new Error(`the operation returned no ${name}: ${JSON.stringify(body)}`);
  }
  return (data as Record<string, unknown>)[name] as T;
}

async function waitFor<T>(what: string, load: () => Promise<T>, done: (value: T) => boolean, timeout = 10_000): Promise<T> {
  const deadline = Date.now() + timeout;
  let last: T | undefined;
  while (Date.now() < deadline) {
    last = await load();
    if (done(last)) return last;
    await Bun.sleep(30);
  }
  throw new Error(`timed out waiting for ${what}: ${JSON.stringify(last)}`);
}

// ---- cron: pure and deterministic, independent of the gateway -----------------

await step('cron: a step expression advances to the next matching minute', async () => {
  expect(cronNext('*/5 * * * *', '2026-01-01T00:02:00Z') === '2026-01-01T00:05:00.000Z', 'expected 00:05');
});

await step('cron: a weekday range skips the weekend', async () => {
  expect(cronNext('0 9 * * 1-5', '2026-01-02T10:00:00Z') === '2026-01-05T09:00:00.000Z', 'expected Monday 09:00');
});

await step('cron: day-of-month and weekday combine with OR when both are restricted', async () => {
  // 2026-01-01 is a Thursday, so the next occurrence is Monday 5 January rather than 1 February.
  expect(cronNext('0 0 1 * 1', '2026-01-01T00:00:00Z') === '2026-01-05T00:00:00.000Z', 'expected OR semantics');
});

await step('cron: 7 is Sunday, so a Sunday-only expression lands on the next Sunday', async () => {
  expect(cronNext('0 0 * * 7', '2026-01-02T00:00:00Z') === '2026-01-04T00:00:00.000Z', 'expected Sunday');
});

await step('cron: a weekday range ending at 7 keeps Sunday', async () => {
  // 2026-01-01 is a Thursday, so Friday 2 January is the next admitted day, not the following Thursday.
  expect(cronNext('0 0 * * 5-7', '2026-01-01T12:00:00Z') === '2026-01-02T00:00:00.000Z', 'expected Friday');
});

await step('cron: an impossible calendar date has no occurrence', async () => {
  expect(cronNext('0 0 31 2 *', '2026-01-01T00:00:00Z') === undefined, '31 February cannot fire');
});

await step('cron: names, six fields and out-of-range values are rejected', async () => {
  const rejects = (expression: string): boolean => {
    try { parseCron(expression); return false; } catch { return true; }
  };
  expect(rejects('0 0 * * MON'), 'weekday names are rejected');
  expect(rejects('* * * * * *'), 'six fields are rejected');
  expect(rejects('60 * * * *'), 'minute 60 is rejected');
});

// ---- gateway: CRUD, authorization, execution, restart -------------------------

const fake = new FakeMcpServer();
const dir = await mkdtemp(join(tmpdir(), 'mcp-mounts-'));
const config = (path: string): GatewayConfig => ({
  node: { id: NODE },
  server: { bind: '127.0.0.1', port: 3997 },
  storage_path: path,
  servers: [{ name: 'fake', url: fake.url, controllers: [CONTROLLER] }],
  mounts: {
    controllers: [CONTROLLER],
    commands: [
      { id: 'hello', command: process.execPath, args: ['-e', 'console.log("mounted hello")'] },
      { id: 'fail', command: process.execPath, args: ['-e', 'console.error("boom"); process.exit(3)'] },
      { id: 'sleep', command: process.execPath, args: ['-e', 'setTimeout(() => {}, 30000)'] },
    ],
    tools: [{ server: 'fake', tool: 'echo' }],
    // Long enough that the timer never fires during the checks; `tick()` drives each pass.
    tick_ms: 60_000,
  },
});

let gateway: McpGateway = await createMcpGateway(config(join(dir, 'store.json')));
const endpoint = 'http://127.0.0.1:3997/onlyne/v3';

const call = async (method: string, path: string, body?: unknown, actor: string = CONTROLLER): Promise<{ status: number; body: Record<string, unknown> }> => {
  const response = await gateway.node.fetch(new Request(endpoint + path, {
    method,
    headers: { 'content-type': 'application/json', 'Onlyne-Actor': actor },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  }));
  const text = await response.text();
  return { status: response.status, body: (text === '' ? {} : JSON.parse(text)) as Record<string, unknown> };
};

const operate = (operation: string, data: Record<string, unknown> = {}, actor: string = CONTROLLER) =>
  call('POST', `${resourcePath(`mounts:${actor === CONTROLLER ? CONTROLLER : actor}`)}/operations`, {
    ...mutation(), operation_id: crypto.randomUUID(), operation, data,
  }, actor);

const runsOf = async (taskId: string): Promise<MountRun[]> =>
  field<MountRun[]>((await operate('runs', { task: taskId, limit: 100 })).body, 'runs');
const taskOf = async (taskId: string): Promise<MountTask> =>
  field<MountTask>((await operate('inspect', { task: taskId })).body, 'task');

try {
  await step('the scheduler resource is registered for the configured controller', async () => {
    const described = await call('GET', resourcePath(RESOURCE_ID));
    const operations = described.body.operations as string[] | undefined;
    expect(described.status === 200, `describe answered ${described.status}`);
    expect(operations?.includes('mount') === true && operations.includes('update') && operations.includes('cancel'), 'operations are incomplete');
  });

  await step('an actor that is not a configured controller cannot see the resource', async () => {
    const described = await call('GET', resourcePath(`mounts:${INTRUDER}`), undefined, INTRUDER);
    expect(described.status === 404, `describe answered ${described.status}`);
  });

  await step('a command mount is validated against the local allowlist', async () => {
    const refused = await operate('mount', {
      schedule: { kind: 'interval', every_ms: 60_000 },
      action: { kind: 'command', command: 'rm -rf /' },
    });
    expect(refused.status === 403, `an unknown command answered ${refused.status}`);
    expect((refused.body.error as { code?: string } | undefined)?.code === 'forbidden', 'the refusal names forbidden');
  });

  await step('an MCP tool outside the configured tool allowlist is refused', async () => {
    const refused = await operate('mount', {
      schedule: { kind: 'interval', every_ms: 60_000 },
      action: { kind: 'mcp', server: 'fake', tool: 'write_data' },
    });
    expect(refused.status === 403, `an unlisted tool answered ${refused.status}`);
  });

  let helloTask = '';
  await step('a fixed-interval command mount fires on the first pass and records the run', async () => {
    const created = await operate('mount', {
      schedule: { kind: 'interval', every_ms: 60_000 },
      action: { kind: 'command', command: 'hello' },
      title: 'hello mount',
    });
    expect(created.status === 201, `mount answered ${created.status}`);
    const task = field<MountTask>(created.body, 'task');
    helloTask = task.id;
    expect(task.state === 'active', 'the task is active');
    expect(task.next_run_at !== null && Date.parse(task.next_run_at) <= Date.now() + 1_000, 'the interval fires immediately');
    await gateway.mounts.tick();
    const runs = await waitFor('a finished run', () => runsOf(helloTask), list => list.some(run => run.state !== 'running'));
    const run = runs.find(candidate => candidate.state !== 'running')!;
    expect(run.state === 'succeeded', `the run state is ${run.state}`);
    expect(run.exit_code === 0, `the exit code is ${run.exit_code}`);
    expect(run.output_summary.includes('mounted hello'), 'the output summary holds stdout');
    expect(run.output_bytes > 0, 'the byte count is recorded');
    const advanced = await taskOf(helloTask);
    expect(advanced.next_run_at !== null && Date.parse(advanced.next_run_at) > Date.now() + 30_000, 'the next run advanced a full period');
  });

  await step('a failing command records its exit code and stderr', async () => {
    const created = await operate('mount', {
      schedule: { kind: 'interval', every_ms: 60_000 },
      action: { kind: 'command', command: 'fail' },
    });
    const task = field<MountTask>(created.body, 'task');
    await gateway.mounts.tick();
    const runs = await waitFor('a finished failing run', () => runsOf(task.id), list => list.some(run => run.state !== 'running'));
    const run = runs.find(candidate => candidate.state !== 'running')!;
    expect(run.state === 'failed', `the run state is ${run.state}`);
    expect(run.exit_code === 3, `the exit code is ${run.exit_code}`);
    expect(run.output_summary.includes('boom'), 'stderr reaches the summary');
    expect(run.error?.code === 'exit_nonzero', `the error code is ${run.error?.code}`);
  });

  await step('an allowlisted MCP tool action runs against the real fake server', async () => {
    const created = await operate('mount', {
      schedule: { kind: 'interval', every_ms: 60_000 },
      action: { kind: 'mcp', server: 'fake', tool: 'echo', arguments: { from: 'mount' } },
    });
    const task = field<MountTask>(created.body, 'task');
    await gateway.mounts.tick();
    const runs = await waitFor('an MCP run', () => runsOf(task.id), list => list.some(run => run.state !== 'running'));
    const run = runs.find(candidate => candidate.state !== 'running')!;
    expect(run.state === 'succeeded', `the run state is ${run.state}`);
    expect(run.exit_code === null, 'an MCP run has no process exit code');
    const recorded = fake.calls.find(callRecord => callRecord.name === 'echo'
      && (callRecord.arguments as { from?: string } | undefined)?.from === 'mount');
    expect(recorded !== undefined, 'the tool call reached the server');
  });

  await step('a result with an undiscovered controller stays in the durable push queue', async () => {
    const runs = await waitFor('a pending push', () => runsOf(helloTask), list =>
      list.some(run => (run.push?.attempts ?? 0) > 0));
    const run = runs.find(candidate => (candidate.push?.attempts ?? 0) > 0)!;
    expect(run.push !== undefined && (run.push.state === 'pending' || run.push.state === 'unknown'), `the push state is ${run.push?.state}`);
    expect(run.push?.error?.startsWith('target-unresolved') === true, `the push error is ${run.push?.error}`);
    expect(run.push?.message_id === run.id, 'the message id is the run id');
  });

  await step('an update is audited and re-arms the schedule without an immediate run', async () => {
    const created = await operate('mount', {
      schedule: { kind: 'interval', every_ms: 1_000 },
      action: { kind: 'command', command: 'hello' },
    });
    const task = field<MountTask>(created.body, 'task');
    const updated = await operate('update', { task: task.id, title: 'retuned', schedule: { kind: 'interval', every_ms: 120_000 } });
    expect(updated.status === 201, `update answered ${updated.status}`);
    const fresh = field<MountTask>(updated.body, 'task');
    expect(fresh.title === 'retuned', 'the title changed');
    expect(Date.parse(fresh.next_run_at!) > Date.now() + 60_000, 'the new period starts from the update');
    const audit = field<MountTask>((await operate('inspect', { task: task.id })).body, 'task').changes;
    expect(audit.some(entry => entry.action === 'update' && entry.actor === CONTROLLER), 'the update is attributed to the controller');
    const refused = await operate('update', {
      task: task.id, action: { kind: 'mcp', server: 'fake', tool: 'write_data' },
    });
    expect(refused.status === 403, `an unlisted tool in update answered ${refused.status}`);
  });

  await step('an occurrence that arrives during a run records a skip, never a second run', async () => {
    const created = await operate('mount', {
      schedule: { kind: 'interval', every_ms: 1_000 },
      action: { kind: 'command', command: 'sleep' },
    });
    const task = field<MountTask>(created.body, 'task');
    await gateway.mounts.tick();
    await waitFor('the sleep run to start', () => runsOf(task.id), list => list.some(run => run.state === 'running'));
    await Bun.sleep(1_100);
    await gateway.mounts.tick();
    const runs = await waitFor('an overlap record', () => runsOf(task.id), list => list.some(run => run.state === 'skipped'));
    const skipped = runs.find(run => run.state === 'skipped')!;
    expect(skipped.error?.code === 'overlap', `the skip names overlap, not ${skipped.error?.code}`);
    const cancelled = await operate('cancel', { task: task.id });
    expect(cancelled.status === 201, `cancel answered ${cancelled.status}`);
    const stopped = await waitFor('the cancelled run to settle', () => runsOf(task.id), list =>
      list.some(run => run.state === 'cancelled'));
    expect(stopped.some(run => run.state === 'cancelled'), 'the running command was cancelled');
    const after = await taskOf(task.id);
    expect(after.state === 'cancelled' && after.next_run_at === null, 'the task is cancelled with no next run');
  });

  await step('a one-shot mount fires once and completes', async () => {
    const created = await operate('mount', {
      schedule: { kind: 'once', at: new Date(Date.now() + 400).toISOString() },
      action: { kind: 'command', command: 'hello' },
    });
    const task = field<MountTask>(created.body, 'task');
    await gateway.mounts.tick();
    expect((await runsOf(task.id)).length === 0, 'it does not fire before its time');
    await Bun.sleep(450);
    await gateway.mounts.tick();
    const runs = await waitFor('the one-shot run', () => runsOf(task.id), list => list.some(run => run.state === 'succeeded'));
    expect(runs.filter(run => run.state !== 'skipped').length === 1, 'exactly one attempt ran');
    const done = await taskOf(task.id);
    expect(done.state === 'completed' && done.next_run_at === null, 'the task completed');
    await gateway.mounts.tick();
    expect((await runsOf(task.id)).length === runs.length, 'a later pass adds nothing');
  });

  await step('resume re-arms a cancelled task', async () => {
    const created = await operate('mount', {
      schedule: { kind: 'interval', every_ms: 60_000 },
      action: { kind: 'command', command: 'hello' },
    });
    const task = field<MountTask>(created.body, 'task');
    await operate('cancel', { task: task.id });
    const resumed = await operate('resume', { task: task.id });
    const fresh = field<MountTask>(resumed.body, 'task');
    expect(fresh.state === 'active', 'the task is active again');
    expect(fresh.next_run_at !== null && Date.parse(fresh.next_run_at) <= Date.now() + 1_000, 'resume fires immediately');
  });

  await step('remove is a soft delete that keeps the runs and audit', async () => {
    const created = await operate('mount', {
      schedule: { kind: 'interval', every_ms: 60_000 },
      action: { kind: 'command', command: 'hello' },
    });
    const task = field<MountTask>(created.body, 'task');
    const removed = await operate('remove', { task: task.id });
    expect(field<MountTask>(removed.body, 'task').state === 'removed', 'the task is removed');
    const listed = field<MountTask[]>((await operate('list')).body, 'tasks');
    expect(listed.every(entry => entry.id !== task.id), 'the default list hides removed tasks');
    const including = field<MountTask[]>((await operate('list', { include_removed: true })).body, 'tasks');
    expect(including.some(entry => entry.id === task.id && entry.state === 'removed'), 'include_removed shows it');
    const audit = field<MountTask>((await operate('inspect', { task: task.id })).body, 'task').changes;
    expect(audit.some(entry => entry.action === 'remove'), 'the removal is audited');
  });

  await step('status lines put the global categories before per-task detail', async () => {
    const lines = await gateway.status();
    expect(lines.length <= 8, `status returned ${lines.length} lines`);
    expect(lines[0]?.startsWith('servers:') === true && lines[0].includes('fake(2)'), `servers line: ${lines[0]}`);
    expect(lines[1]?.startsWith('mounts:') === true, `mounts line: ${lines[1]}`);
    expect(lines[2]?.startsWith('push:') === true, `push line: ${lines[2]}`);
    expect(lines[3]?.startsWith('last:') === true, `last line: ${lines[3]}`);
    expect(lines.slice(4).some(line => line.includes('next=')) || !lines[4]?.startsWith('  '), 'task rows follow the categories');
  });

  await step('a stopped run is recorded as interrupted and survives a restart', async () => {
    const created = await operate('mount', {
      schedule: { kind: 'interval', every_ms: 60_000 },
      action: { kind: 'command', command: 'sleep' },
    });
    const task = field<MountTask>(created.body, 'task');
    await gateway.mounts.tick();
    await waitFor('the long run to start', () => runsOf(task.id), list => list.some(run => run.state === 'running'));
    await gateway.stop();
    gateway = await createMcpGateway(config(join(dir, 'store.json')));
    const listed = field<MountTask[]>((await operate('list')).body, 'tasks');
    expect(listed.some(entry => entry.id === task.id), 'the task survives the restart');
    const runs = await runsOf(task.id);
    expect(runs.some(run => run.state === 'interrupted' && run.error?.code === 'interrupted'), 'the run is recorded as interrupted');
    expect(runs.every(run => run.state !== 'running'), 'no run is left looking active');
    // The controller is still not discoverable, so the interrupted result waits in the queue.
    await gateway.mounts.tick();
    const queued = await waitFor('a queued interrupted result', () => runsOf(task.id), list =>
      list.some(run => run.state === 'interrupted' && (run.push?.attempts ?? 0) > 0));
    expect(queued.some(run => run.push?.message_id === run.id), 'the interrupted result carries a stable message id');
  });
} finally {
  await gateway.stop();
  await fake.stop();
  await rm(dir, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`mcp-gateway mounts verify: ${failures} check(s) failed`);
  process.exit(1);
}
console.log('mcp-gateway mounts verify: all checks passed');
