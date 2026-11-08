/**
 * OECP v1 and RFC 0019 Sandbox Compute Types and Schema Definitions.
 */

export type LeaseState = 'provisioning' | 'running' | 'stopped' | 'destroyed' | 'failed';

export interface ContainerSpec {
  image: string;
  image_pull_policy?: 'always' | 'if_not_present' | 'never';
  command?: string[];
  args?: string[];
  workdir?: string;
  env?: Record<string, string>;
  expose_env?: string[];
  mounts?: Array<{
    type: 'bind' | 'volume' | 'tmpfs';
    host_path?: string;
    container_path: string;
    read_only?: boolean;
  }>;
}

export interface ResourceConstraints {
  arch?: 'arm64' | 'x86_64' | 'any';
  cpus?: number;
  memory_mb?: number;
  gpu?: {
    model?: string;
    count: number;
  };
  shm_size_mb?: number;
}

export interface PortRequirement {
  name: string;
  container_port: number;
  protocol?: 'tcp' | 'udp';
  public?: boolean;
  preferred_host_port?: number;
}

export interface NetworkSpec {
  mode?: 'bridge' | 'host' | 'overlay' | 'none';
  nat_ports?: PortRequirement[];
  egress_policy?: 'allow_all' | 'intranet_only' | 'air_gapped';
}

export interface ProviderMeta {
  selector?: string;
  fallback?: string[];
  region?: string;
  driver_options?: Record<string, unknown>;
}

export interface LeaseControl {
  ttl_ms?: number;
  max_ttl_ms?: number;
  idle_timeout_ms?: number;
  retention_ms?: number;
}

export interface AcquireSpec {
  name: string;
  container: ContainerSpec;
  resources?: ResourceConstraints;
  network?: NetworkSpec;
  provider_meta?: ProviderMeta;
  lease?: LeaseControl;
  purpose?: string;
}

export interface DirectConnection {
  exec_transport?: 'ssh' | 'docker_exec' | 'k8s_exec';
  ssh_endpoint?: string;
  auth?: {
    type: 'ephemeral_key' | 'password' | 'token';
    private_key?: string;
    user?: string;
    token?: string;
  };
  endpoints?: Record<string, string>;
  portal_urls?: Record<string, string>;
}

export interface SandboxDescriptor {
  lease_id: string;
  name: string;
  resource_ref?: { owner: string; id: string };
  state: LeaseState;
  actual_provider: string;
  created_at: number;
  expires_at: number;
  retention_until: number;
  connection?: DirectConnection;
  resolved_env?: Record<string, string>;
  allocated?: {
    arch: string;
    container_id: string;
    host_ip: string;
  };
}

export interface LeaseRecord {
  lease_id: string;
  name: string;
  submitter: string;
  spec: AcquireSpec;
  state: LeaseState;
  provider: string;
  container_id?: string;
  created_at: number;
  total_ttl_ms: number;
  max_ttl_ms: number;
  expires_at: number;
  retention_until: number;
  descriptor?: SandboxDescriptor;
  error?: string;
}
