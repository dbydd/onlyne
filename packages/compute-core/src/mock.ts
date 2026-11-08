import type { SandboxDriver, DriverAllocation } from './driver.ts';
import type { AcquireSpec, DirectConnection, LeaseState } from './types.ts';

export class MockSandboxDriver implements SandboxDriver {
  readonly id: string;
  readonly supportedArchs: ReadonlyArray<'arm64' | 'x86_64'>;
  readonly containers = new Map<string, { state: LeaseState; spec: AcquireSpec; leaseId: string }>();

  constructor(id = 'mock-provider', supportedArchs: ReadonlyArray<'arm64' | 'x86_64'> = ['arm64', 'x86_64']) {
    this.id = id;
    this.supportedArchs = supportedArchs;
  }

  async provision(leaseId: string, spec: AcquireSpec): Promise<DriverAllocation> {
    const container_id = `mock-cnt-${leaseId}`;
    this.containers.set(container_id, { state: 'running', spec, leaseId });

    const endpoints: Record<string, string> = {};
    const portal_urls: Record<string, string> = {};
    for (const port of spec.network?.nat_ports ?? []) {
      const assigned = port.preferred_host_port || (30000 + Math.floor(Math.random() * 5000));
      endpoints[port.name] = `http://127.0.0.1:${assigned}`;
      if (port.name === 'vnc-desktop' || port.name === 'desktop') {
        portal_urls['desktop'] = `http://127.0.0.1:${assigned}/vnc.html?autoconnect=true`;
      }
    }

    const connection: DirectConnection = {
      exec_transport: 'ssh',
      ssh_endpoint: '127.0.0.1:2222',
      auth: {
        type: 'ephemeral_key',
        private_key: '-----BEGIN OPENSSH PRIVATE KEY-----\nMOCK_KEY\n-----END OPENSSH PRIVATE KEY-----',
        user: 'root',
      },
      endpoints,
      portal_urls,
    };

    const resolved_env: Record<string, string> = {};
    for (const envKey of spec.container.expose_env ?? []) {
      resolved_env[envKey] = `mock_val_${envKey.toLowerCase()}`;
    }

    return {
      container_id,
      provider: this.id,
      arch: spec.resources?.arch && spec.resources.arch !== 'any' ? spec.resources.arch : 'arm64',
      host_ip: '127.0.0.1',
      connection,
      resolved_env,
    };
  }

  async start(containerId: string): Promise<DirectConnection | undefined> {
    const item = this.containers.get(containerId);
    if (!item) throw new Error(`Container ${containerId} not found`);
    item.state = 'running';
    return undefined;
  }

  async stop(containerId: string): Promise<void> {
    const item = this.containers.get(containerId);
    if (!item) return;
    item.state = 'stopped';
  }

  async destroy(containerId: string): Promise<void> {
    const item = this.containers.get(containerId);
    if (!item) return;
    item.state = 'destroyed';
    this.containers.delete(containerId);
  }

  async status(containerId: string): Promise<{ state: LeaseState; error?: string }> {
    const item = this.containers.get(containerId);
    if (!item) return { state: 'destroyed' };
    return { state: item.state };
  }
}
