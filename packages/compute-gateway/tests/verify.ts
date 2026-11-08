import { OnlyneError } from '@onlyne/lib';
import { MemoryStorage } from '@onlyne/lib/memory';
import { MockSandboxDriver } from '@onlyne/compute-core';
import { ComputeBrokerService, ComputeGatewayNode } from '../src/index.ts';

let failures = 0;
const ok = (label: string, condition: boolean) => {
  if (condition) console.log(`ok   ${label}`);
  else { failures += 1; console.error(`FAIL ${label}`); }
};

let virtualTime = 1_000_000;
const now = () => virtualTime;

const storage = new MemoryStorage();
const driver = new MockSandboxDriver('cloud-provider');
const service = new ComputeBrokerService({
  nodeId: 'urn:onlyne:gateway:compute',
  storage,
  driver,
  now,
});

// 1. Acquire
const desc1 = await service.acquire('urn:onlyne:agent:alice', {
  name: 'test-runner-1',
  container: {
    image: 'debian:12-slim',
    expose_env: ['AGENT_TOKEN'],
  },
  network: {
    nat_ports: [{ name: 'vnc-desktop', container_port: 6080 }],
  },
  lease: {
    ttl_ms: 10_000,
    max_ttl_ms: 30_000,
    retention_ms: 50_000,
  },
});

ok('acquire returns running descriptor', desc1.state === 'running');
ok('acquire sets lease_id', typeof desc1.lease_id === 'string' && desc1.lease_id.startsWith('cls_'));
ok('acquire sets expires_at correctly', desc1.expires_at === virtualTime + 10_000);
ok('acquire sets retention_until correctly', desc1.retention_until === virtualTime + 60_000);
ok('acquire provides direct connection endpoints', String(desc1.connection?.portal_urls?.['desktop']).includes('/vnc.html'));

// Idempotent acquire
const desc1_re = await service.acquire('urn:onlyne:agent:alice', {
  name: 'test-runner-1',
  container: { image: 'debian:12-slim' },
});
ok('idempotent acquire returns identical lease_id', desc1_re.lease_id === desc1.lease_id);

// Cross-owner acquire collision -> forbidden
let crossOwnerError = '';
try {
  await service.acquire('urn:onlyne:agent:mallory', {
    name: 'test-runner-1',
    container: { image: 'debian:12-slim' },
  });
} catch (err: unknown) {
  crossOwnerError = err instanceof OnlyneError ? err.error.code : '';
}
ok('cross-owner acquire with same name is rejected with forbidden', crossOwnerError === 'forbidden');

// Initial acquire exceeding max_ttl_ms -> capacity_exceeded without provisioning
let overCapError = '';
try {
  await service.acquire('urn:onlyne:agent:alice', {
    name: 'over-cap-runner',
    container: { image: 'debian:12-slim' },
    lease: { ttl_ms: 60_000, max_ttl_ms: 30_000 },
  });
} catch (err: unknown) {
  overCapError = err instanceof OnlyneError ? err.error.code : '';
}
ok('acquire exceeding max_ttl_ms is rejected with capacity_exceeded', overCapError === 'capacity_exceeded');

// 2. Renew
virtualTime += 5_000;
const renewed = await service.renew('urn:onlyne:agent:alice', desc1.lease_id, 10_000);
// initial was 1_000_000 + 10_000 = 1_010_000. At 1_005_000, Math.max(1_010_000, 1_005_000) + 10_000 = 1_020_000.
ok('renew extends expires_at', renewed.expires_at === 1_020_000);

let renewFailed = false;
try {
  // Exceed max_ttl_ms (10_000 + 10_000 + 20_000 > 30_000)
  await service.renew('urn:onlyne:agent:alice', desc1.lease_id, 20_000);
} catch {
  renewFailed = true;
}
ok('renew past max_ttl_ms is rejected', renewFailed);

// 3. Release
const released = await service.release('urn:onlyne:agent:alice', desc1.lease_id);
ok('release transitions state to stopped', released.state === 'stopped');

// Re-acquire previously stopped lease restarts it
virtualTime += 2_000;
const restarted = await service.acquire('urn:onlyne:agent:alice', {
  name: 'test-runner-1',
  container: { image: 'debian:12-slim' },
  lease: { ttl_ms: 5_000, retention_ms: 20_000 },
});
ok('re-acquire stopped lease brings state back to running', restarted.state === 'running');

// 4. Safety Net Sweep
// Advance past expires_at -> should be auto-stopped
virtualTime += 10_000;
let sweepRes = await service.sweep();
ok('sweep auto-stops expired lease', sweepRes.stopped.includes(desc1.lease_id));

// Advance past retention_until -> should be auto-destroyed
virtualTime += 100_000;
sweepRes = await service.sweep();
ok('sweep auto-destroys expired retention lease', sweepRes.destroyed.includes(desc1.lease_id));

// 5. ComputeGatewayNode wrapper and dispatch
const gateway = new ComputeGatewayNode({
  config: {
    node: { id: 'urn:onlyne:gateway:compute' },
    server: { bind: '127.0.0.1', port: 3998 },
    storage: {},
  },
  storage: new MemoryStorage(),
  driver: new MockSandboxDriver('gw-drv'),
});

const gwDesc = (await gateway.handleCompute('urn:onlyne:agent:bob', {
  operation: 'acquire',
  name: 'box-via-gw',
  container: { image: 'alpine:latest' },
})) as unknown as { state: string; lease_id: string };

ok('gateway handleCompute routes acquire', gwDesc.state === 'running');

const gwStatus = (await gateway.handleCompute('urn:onlyne:agent:bob', {
  operation: 'status',
  lease_id: gwDesc.lease_id,
})) as unknown as { state: string };

ok('gateway handleCompute routes status', gwStatus.state === 'running');

const gwDestroyed = (await gateway.handleCompute('urn:onlyne:agent:bob', {
  operation: 'destroy',
  lease_id: gwDesc.lease_id,
})) as unknown as { state: string };

ok('gateway handleCompute routes destroy', gwDestroyed.state === 'destroyed');

await gateway.close();

// 6. Inbound compute messages execute through runtime.inject and answer on the thread
const injGateway = new ComputeGatewayNode({
  config: {
    node: { id: 'urn:onlyne:gateway:compute' },
    server: { bind: '127.0.0.1', port: 3999 },
    storage: {},
  },
  storage: new MemoryStorage(),
  driver: new MockSandboxDriver('inj-drv'),
  clock: {
    setTimer: (_ms, run) => setTimeout(run, 0),
    clearTimer: handle => clearTimeout(handle as ReturnType<typeof setTimeout>),
  },
});
const admitCompute = async (
  id: string,
  meta: Record<string, unknown>,
  plan: { response: 'none' | 'async' | 'await'; execution: 'existing' | 'new' } = { response: 'async', execution: 'existing' },
) => injGateway.node.messages.admit({ actor: 'urn:onlyne:agent:client' }, {
  idempotency_key: `inj-${id}`,
  retry_until: new Date(Date.now() + 60_000).toISOString(),
  target_endpoint: 'urn:onlyne:gateway:compute',
  plan,
  message: {
    id, sender: 'urn:onlyne:agent:client', delivery_kind: 'compute',
    msg: 'compute request', meta: meta as never,
  },
});

const injReceipt = await admitCompute('inj-acq-1', {
  operation: 'acquire', name: 'inj-box', container: { image: 'alpine:latest' },
  work: { unit: 'unit-7', attempt: 2, parent: null },
});

const delay = (ms: number): Promise<void> => {
  const { promise, resolve } = Promise.withResolvers<void>();
  setTimeout(resolve, ms);
  return promise;
};
const waitForReply = async (
  thread: { owner: string; id: string }, id: string,
): Promise<{ msg: string; work?: unknown } | undefined> => {
  for (let i = 0; i < 100; i += 1) {
    const page = await injGateway.node.messages.list(injGateway.node.auth, thread, { attention: 'thread' });
    const row = page.items.find(item =>
      item.message.meta?.in_reply_to?.id === id && item.message.sender === 'urn:onlyne:gateway:compute');
    if (row !== undefined) return { msg: row.message.msg, work: row.message.meta?.work };
    await delay(20);
  }
  return undefined;
};

const injReply = await waitForReply(injReceipt.thread_ref, 'inj-acq-1');

const injPayload = injReply === undefined ? undefined : JSON.parse(injReply.msg) as { state?: string };
ok('injected compute message acquires and replies on the thread', injPayload?.state === 'running');
ok('the compute reply preserves meta.work',
  JSON.stringify((injReply as { work?: { unit?: string; attempt?: number } } | undefined)?.work)
    === JSON.stringify({ unit: 'unit-7', attempt: 2, parent: null }));

// A compute message whose execution fails answers { ok:false, error } instead of crashing the loop
const badReceipt = await admitCompute('inj-bad-1', { operation: 'explode' });
const badReply = await waitForReply(badReceipt.thread_ref, 'inj-bad-1');
const badPayload = badReply === undefined
  ? undefined : JSON.parse(badReply.msg) as { ok: boolean; error: { code: string } };
ok('a failed compute operation replies ok:false with the error code',
  badPayload?.ok === false && badPayload.error.code === 'invalid_request');

// response:none executes without a reply
const quietReceipt = await admitCompute(
  'inj-quiet-1', { operation: 'status', lease_id: 'cls_missing' }, { response: 'none', execution: 'existing' });
await delay(150);
const quietPage = await injGateway.node.messages.list(injGateway.node.auth, quietReceipt.thread_ref, { attention: 'thread' });
ok('response:none compute traffic executes without a reply',
  !quietPage.items.some(item => item.message.meta?.in_reply_to?.id === 'inj-quiet-1'));

await injGateway.close();

if (failures > 0) {
  console.error(`compute-gateway verify: ${failures} failed`);
  process.exit(1);
} else {
  console.log('compute-gateway verify: all checks passed');
}
