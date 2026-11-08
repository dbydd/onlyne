import type { AcquireSpec, DirectConnection, LeaseState } from './types.ts';

export interface DriverAllocation {
  container_id: string;
  provider: string;
  arch: string;
  host_ip: string;
  connection?: DirectConnection;
  resolved_env?: Record<string, string>;
}

export interface SandboxDriver {
  readonly id: string;
  readonly supportedArchs: ReadonlyArray<'arm64' | 'x86_64'>;

  provision(leaseId: string, spec: AcquireSpec): Promise<DriverAllocation>;
  start(containerId: string): Promise<DirectConnection | undefined>;
  stop(containerId: string): Promise<void>;
  destroy(containerId: string): Promise<void>;
  status(containerId: string): Promise<{ state: LeaseState; error?: string }>;
}
