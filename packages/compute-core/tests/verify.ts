import { MockSandboxDriver } from '../src/index.ts';

let failures = 0;
const ok = (label: string, condition: boolean) => {
  if (condition) console.log(`ok   ${label}`);
  else { failures += 1; console.error(`FAIL ${label}`); }
};

const driver = new MockSandboxDriver('test-drv');
const alloc = await driver.provision('lease-1', {
  name: 'test-box',
  container: {
    image: 'debian:12-slim',
    expose_env: ['TOKEN'],
  },
  network: {
    nat_ports: [{ name: 'vnc-desktop', container_port: 6080 }],
  },
});

ok('driver allocates container_id', alloc.container_id === 'mock-cnt-lease-1');
ok('driver assigns provider id', alloc.provider === 'test-drv');
ok('driver generates desktop portal url', String(alloc.connection?.portal_urls?.['desktop']).includes('/vnc.html'));
ok('driver resolves requested env', alloc.resolved_env?.['TOKEN'] === 'mock_val_token');

let status = await driver.status(alloc.container_id);
ok('initial status is running', status.state === 'running');

await driver.stop(alloc.container_id);
status = await driver.status(alloc.container_id);
ok('status after stop is stopped', status.state === 'stopped');

await driver.destroy(alloc.container_id);
status = await driver.status(alloc.container_id);
ok('status after destroy is destroyed', status.state === 'destroyed');

if (failures > 0) {
  console.error(`compute-core verify: ${failures} failed`);
  process.exit(1);
} else {
  console.log('compute-core verify: all checks passed');
}
