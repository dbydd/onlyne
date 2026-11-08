import type { SandboxDriver, DriverAllocation } from './driver.ts';
import type { AcquireSpec, DirectConnection, LeaseState } from './types.ts';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';

const execFileAsync = promisify(execFile);

export interface OrbstackMachineDriverOptions {
  id?: string;
  orbctlPath?: string;
  defaultImage?: string;
  readyTimeoutMs?: number;
}

interface MachineRecord {
  name: string;
  state: string;
  config?: {
    arch?: string;
    cpu_limit?: number;
    memory_limit_mib?: number;
  };
}

export class OrbstackMachineDriver implements SandboxDriver {
  readonly id: string;
  readonly supportedArchs: ReadonlyArray<'arm64' | 'x86_64'> = ['arm64'];
  readonly orbctlPath: string;
  readonly defaultImage: string;
  readonly readyTimeoutMs: number;

  constructor(options: OrbstackMachineDriverOptions = {}) {
    this.id = options.id ?? 'orbstack-isolated-vm';
    this.orbctlPath = options.orbctlPath ?? 'orbctl';
    this.defaultImage = options.defaultImage ?? 'debian:12';
    this.readyTimeoutMs = options.readyTimeoutMs ?? 60_000;
  }

  private async runOrbctl(argv: string[], timeoutMs = 60_000): Promise<{ stdout: string; stderr: string }> {
    return execFileAsync(this.orbctlPath, argv, {
      timeout: timeoutMs,
      maxBuffer: 4 * 1024 * 1024,
    });
  }

  private async listMachines(): Promise<MachineRecord[]> {
    try {
      const { stdout } = await this.runOrbctl(['list', '-f', 'json']);
      if (!stdout.trim()) return [];
      const parsed = JSON.parse(stdout);
      return Array.isArray(parsed) ? (parsed as MachineRecord[]) : [];
    } catch {
      return [];
    }
  }

  private async recordOf(name: string): Promise<MachineRecord | undefined> {
    const list = await this.listMachines();
    return list.find(m => m.name === name);
  }

  private async waitReachable(name: string): Promise<void> {
    const deadline = Date.now() + this.readyTimeoutMs;
    while (Date.now() < deadline) {
      try {
        const { stdout } = await this.runOrbctl(['run', '-m', name, 'true'], 5_000);
        return;
      } catch {
        await new Promise(r => setTimeout(r, 1_000));
      }
    }
    throw new Error(`OrbStack machine "${name}" did not become reachable in time.`);
  }

  async provision(leaseId: string, spec: AcquireSpec): Promise<DriverAllocation> {
    // Sanitize machine name (alphanumeric + hyphen, lowercase)
    const machineName = `omd-${spec.name.toLowerCase().replace(/[^a-z0-9-]/g, '-').slice(0, 30)}`;
    const image = spec.container.image.includes('/')
      ? spec.container.image.split('/').pop()!
      : spec.container.image;
    const finalImage = image.startsWith('debian') || image.startsWith('ubuntu') || image.startsWith('alpine')
      ? image
      : this.defaultImage;

    const existing = await this.recordOf(machineName);

    if (!existing) {
      const argv = ['create', '--isolated', '--arch', 'arm64'];

      if (spec.resources?.cpus) {
        argv.push('--cpus', String(spec.resources.cpus));
      }
      if (spec.resources?.memory_mb) {
        argv.push('--memory', `${spec.resources.memory_mb}M`);
      }

      for (const m of spec.container.mounts ?? []) {
        if (m.host_path) {
          argv.push('--mount', `${m.host_path}:${m.container_path}`);
        }
      }

      argv.push(finalImage, machineName);
      await this.runOrbctl(argv, this.readyTimeoutMs);
    } else if (existing.state !== 'running') {
      await this.runOrbctl(['start', machineName], this.readyTimeoutMs);
    }

    await this.waitReachable(machineName);

    // Isolated machines resolve via <name>.orb.local in host DNS
    const host_ip = `${machineName}.orb.local`;
    const endpoints: Record<string, string> = {};
    const portal_urls: Record<string, string> = {};

    for (const p of spec.network?.nat_ports ?? []) {
      const port = p.container_port;
      const url = `http://${host_ip}:${port}`;
      endpoints[p.name] = url;
      if (p.name === 'vnc-desktop' || p.name === 'desktop') {
        portal_urls['desktop'] = `${url}/vnc.html?autoconnect=true`;
      }
    }

    const connection: DirectConnection = {
      exec_transport: 'ssh',
      ssh_endpoint: `${host_ip}:22`,
      auth: {
        type: 'ephemeral_key',
        user: 'root',
      },
      endpoints,
      portal_urls,
    };

    return {
      container_id: machineName,
      provider: this.id,
      arch: 'arm64',
      host_ip,
      connection,
      resolved_env: spec.container.env ?? {},
    };
  }

  async start(containerId: string): Promise<DirectConnection | undefined> {
    await this.runOrbctl(['start', containerId], this.readyTimeoutMs);
    await this.waitReachable(containerId);
    return undefined;
  }

  async stop(containerId: string): Promise<void> {
    const existing = await this.recordOf(containerId);
    if (!existing || existing.state !== 'running') return;
    await this.runOrbctl(['stop', containerId], 30_000);
  }

  async destroy(containerId: string): Promise<void> {
    const existing = await this.recordOf(containerId);
    if (!existing) return;
    await this.runOrbctl(['delete', '--force', containerId], 30_000);
  }

  async status(containerId: string): Promise<{ state: LeaseState; error?: string }> {
    const rec = await this.recordOf(containerId);
    if (!rec) return { state: 'destroyed' };
    if (rec.state === 'running') return { state: 'running' };
    if (rec.state === 'stopped') return { state: 'stopped' };
    return { state: 'failed' };
  }
}
