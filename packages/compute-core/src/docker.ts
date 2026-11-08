import type { SandboxDriver, DriverAllocation } from './driver.ts';
import type { AcquireSpec, DirectConnection, LeaseState } from './types.ts';
import { Socket } from 'node:net';

export interface DockerDriverOptions {
  socketPath?: string;
  id?: string;
}

export class DockerSandboxDriver implements SandboxDriver {
  readonly id: string;
  readonly supportedArchs: ReadonlyArray<'arm64' | 'x86_64'> = ['arm64', 'x86_64'];
  readonly socketPath: string;

  constructor(options: DockerDriverOptions = {}) {
    this.id = options.id ?? 'docker-local';
    this.socketPath = options.socketPath ?? '/var/run/docker.sock';
  }

  private async request(method: string, path: string, body?: unknown): Promise<{ status: number; data: unknown }> {
    return new Promise((resolve, reject) => {
      const socket = new Socket();
      socket.connect(this.socketPath, () => {
        const payload = body ? JSON.stringify(body) : '';
        let req = `${method} ${path} HTTP/1.1\r\n`;
        req += `Host: localhost\r\n`;
        req += `Connection: close\r\n`;
        if (payload) {
          req += `Content-Type: application/json\r\n`;
          req += `Content-Length: ${Buffer.byteLength(payload)}\r\n`;
        }
        req += `\r\n`;
        if (payload) req += payload;
        socket.write(req);
      });

      let rawResponse = '';
      socket.on('data', chunk => {
        rawResponse += chunk.toString('latin1');
      });

      socket.on('end', () => {
        const headerEnd = rawResponse.indexOf('\r\n\r\n');
        if (headerEnd === -1) {
          resolve({ status: 500, data: null });
          return;
        }
        const headers = rawResponse.slice(0, headerEnd);
        const statusMatch = headers.match(/HTTP\/1\.[01] (\d+)/);
        const status = statusMatch && statusMatch[1] ? Number(statusMatch[1]) : 500;
        let rawBody = rawResponse.slice(headerEnd + 4);

        if (headers.includes('Transfer-Encoding: chunked')) {
          let dechunked = '';
          let cur = rawBody;
          while (cur.length > 0) {
            const lineEnd = cur.indexOf('\r\n');
            if (lineEnd === -1) break;
            const chunkSizeHex = cur.slice(0, lineEnd).trim();
            const chunkSize = parseInt(chunkSizeHex, 16);
            if (isNaN(chunkSize) || chunkSize === 0) break;
            const chunkData = cur.slice(lineEnd + 2, lineEnd + 2 + chunkSize);
            dechunked += chunkData;
            cur = cur.slice(lineEnd + 2 + chunkSize + 2);
          }
          rawBody = dechunked;
        }

        try {
          const parsed = rawBody ? JSON.parse(rawBody) : null;
          resolve({ status, data: parsed });
        } catch {
          resolve({ status, data: rawBody });
        }
      });

      socket.on('error', reject);
    });
  }

  async provision(leaseId: string, spec: AcquireSpec): Promise<DriverAllocation> {
    const name = `oecp-${leaseId}`;
    const image = spec.container.image;

    // 1. Create container
    const exposedPorts: Record<string, Record<string, string>> = {};
    const portBindings: Record<string, Array<{ HostPort: string }>> = {};
    for (const p of spec.network?.nat_ports ?? []) {
      const portKey = `${p.container_port}/${p.protocol ?? 'tcp'}`;
      exposedPorts[portKey] = {};
      portBindings[portKey] = [{ HostPort: p.preferred_host_port ? String(p.preferred_host_port) : '' }];
    }

    const envList: string[] = [];
    if (spec.container.env) {
      for (const [k, v] of Object.entries(spec.container.env)) {
        envList.push(`${k}=${v}`);
      }
    }

    const createBody = {
      Image: image,
      Cmd: spec.container.args ?? (spec.container.command ? spec.container.command : ['sleep', 'infinity']),
      WorkingDir: spec.container.workdir ?? '/workspace',
      Env: envList,
      ExposedPorts: exposedPorts,
      HostConfig: {
        PortBindings: portBindings,
        ShmSize: spec.resources?.shm_size_mb ? spec.resources.shm_size_mb * 1024 * 1024 : 64 * 1024 * 1024,
      },
      Labels: {
        'onlyne.lease_id': leaseId,
        'onlyne.managed': 'true',
      },
    };

    const res = await this.request('POST', `/containers/create?name=${encodeURIComponent(name)}`, createBody);
    if (res.status !== 201) {
      throw new Error(`Failed to create docker container: ${JSON.stringify(res.data)}`);
    }
    const container_id = (res.data as { Id: string }).Id;

    // 2. Start container
    const startRes = await this.request('POST', `/containers/${container_id}/start`);
    if (startRes.status !== 204 && startRes.status !== 304) {
      throw new Error(`Failed to start docker container: ${JSON.stringify(startRes.data)}`);
    }

    // 3. Inspect container to resolve dynamic ports and host ip
    const inspectRes = await this.request('GET', `/containers/${container_id}/json`);
    const inspect = inspectRes.data as {
      NetworkSettings: {
        IPAddress: string;
        Ports: Record<string, Array<{ HostIp: string; HostPort: string }>>;
      };
    };

    const endpoints: Record<string, string> = {};
    const portal_urls: Record<string, string> = {};
    for (const p of spec.network?.nat_ports ?? []) {
      const portKey = `${p.container_port}/${p.protocol ?? 'tcp'}`;
      const bindings = inspect.NetworkSettings?.Ports?.[portKey];
      const hostPort = bindings && bindings[0] ? bindings[0].HostPort : String(p.container_port);
      const url = `http://127.0.0.1:${hostPort}`;
      endpoints[p.name] = url;
      if (p.name === 'vnc-desktop' || p.name === 'desktop') {
        portal_urls['desktop'] = `${url}/vnc.html?autoconnect=true`;
      }
    }

    const connection: DirectConnection = {
      exec_transport: 'docker_exec',
      endpoints,
      portal_urls,
    };

    return {
      container_id,
      provider: this.id,
      arch: spec.resources?.arch && spec.resources.arch !== 'any' ? spec.resources.arch : 'arm64',
      host_ip: '127.0.0.1',
      connection,
      resolved_env: spec.container.env ?? {},
    };
  }

  async start(containerId: string): Promise<DirectConnection | undefined> {
    await this.request('POST', `/containers/${containerId}/start`);
    return undefined;
  }

  async stop(containerId: string): Promise<void> {
    await this.request('POST', `/containers/${containerId}/stop?t=5`);
  }

  async destroy(containerId: string): Promise<void> {
    await this.request('DELETE', `/containers/${containerId}?force=true&v=true`);
  }

  async status(containerId: string): Promise<{ state: LeaseState; error?: string }> {
    const res = await this.request('GET', `/containers/${containerId}/json`);
    if (res.status === 404) return { state: 'destroyed' };
    const data = res.data as { State?: { Status: string; Running: boolean } };
    if (!data.State) return { state: 'failed' };
    if (data.State.Running) return { state: 'running' };
    if (data.State.Status === 'exited' || data.State.Status === 'stopped') return { state: 'stopped' };
    return { state: 'failed' };
  }
}
