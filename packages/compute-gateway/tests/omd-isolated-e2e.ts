import { MemoryStorage } from '@onlyne/lib/memory';
import { OrbstackMachineDriver } from '@onlyne/compute-core';
import { ComputeBrokerService, ComputeGatewayNode } from '../src/index.ts';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';

const exec = promisify(execFile);

let failures = 0;
const ok = (label: string, condition: boolean) => {
  if (condition) console.log(`ok   ${label}`);
  else { failures += 1; console.error(`FAIL ${label}`); }
};

const storage = new MemoryStorage();
const driver = new OrbstackMachineDriver({ id: 'orbstack-isolated-vm' });
const service = new ComputeBrokerService({
  nodeId: 'urn:onlyne:gateway:compute',
  storage,
  driver,
});

const tempWorkspace = await mkdtemp(join(tmpdir(), 'omd-verify-'));
const machineName = `vfy-${Date.now().toString().slice(-4)}`;

try {
  // 1. Acquire via compute broker service (matching omd ComputeSpec requirements)
  const descriptor = await service.acquire('urn:onlyne:agent:omd-runner', {
    name: machineName,
    container: {
      image: 'debian:12',
      mounts: [{ type: 'bind', host_path: tempWorkspace, container_path: '/workspace' }],
    },
    resources: { cpus: 2, memory_mb: 2048 },
    network: {
      nat_ports: [{ name: 'vnc-desktop', container_port: 6080 }],
    },
    lease: { ttl_ms: 60_000, retention_ms: 120_000 },
    purpose: 'omd-issue-runner validation run',
  });

  ok('acquire returns running isolated machine', descriptor.state === 'running');
  ok('descriptor provides ssh endpoint on orb.local', String(descriptor.connection?.ssh_endpoint).includes('.orb.local'));
  ok('descriptor provides desktop endpoint', String(descriptor.connection?.portal_urls?.['desktop']).includes('/vnc.html'));

  const internalName = descriptor.allocated?.container_id;
  ok('internal container_id allocated', typeof internalName === 'string');

  // Verify execution and isolation inside the VM
  const unameRes = await exec('orbctl', ['run', '-m', internalName!, 'uname', '-m']);
  ok('guest architecture is arm64', unameRes.stdout.trim() === 'aarch64');

  // Verify mount and host workspace interaction
  await exec('orbctl', ['run', '-m', internalName!, 'sh', '-c', 'echo "omd-ok" > /workspace/flag.txt']);
  const flagText = await Bun.file(join(tempWorkspace, 'flag.txt')).text();
  ok('guest mount writes back to host workspace', flagText.trim() === 'omd-ok');

  // 2. Status inspection
  const status = await service.status('urn:onlyne:agent:omd-runner', descriptor.lease_id);
  ok('status returns descriptor matching lease', !Array.isArray(status) && status.lease_id === descriptor.lease_id);

  // 3. Release (Stop machine, retain disk)
  const released = await service.release('urn:onlyne:agent:omd-runner', descriptor.lease_id);
  ok('release stops machine', released.state === 'stopped');

  const checkStopped = await driver.status(internalName!);
  ok('driver confirms machine is stopped', checkStopped.state === 'stopped');

  // 4. Destroy (Physical removal of VM)
  const destroyed = await service.destroy('urn:onlyne:agent:omd-runner', descriptor.lease_id);
  ok('destroy marks lease destroyed', destroyed.state === 'destroyed');

  const checkDestroyed = await driver.status(internalName!);
  ok('driver confirms machine is destroyed', checkDestroyed.state === 'destroyed');
} finally {
  await rm(tempWorkspace, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`omd-isolated-e2e: ${failures} failed`);
  process.exit(1);
} else {
  console.log('omd-isolated-e2e: all checks passed');
}
