import { OnlyneError, type Storage } from '@onlyne/lib';
import type { AcquireSpec, DirectConnection, LeaseRecord, LeaseState, SandboxDescriptor, SandboxDriver } from '@onlyne/compute-core';
import { LeaseLedger } from './ledger.ts';

const DEFAULT_TTL_MS = 30 * 60 * 1000;       // 30 min
const DEFAULT_MAX_TTL_MS = 4 * 60 * 60 * 1000; // 4 hours
const DEFAULT_RETENTION_MS = 24 * 60 * 60 * 1000; // 24 hours

export interface ComputeBrokerOptions {
  nodeId: string;
  storage: Storage;
  driver: SandboxDriver;
  now?: () => number;
  maxTtlMs?: number;
  onExpire?: (record: LeaseRecord) => void | Promise<void>;
}

export class ComputeBrokerService {
  readonly nodeId: string;
  readonly ledger: LeaseLedger;
  readonly driver: SandboxDriver;
  readonly now: () => number;
  readonly maxTtlMs: number;
  private readonly onExpire?: (record: LeaseRecord) => void | Promise<void>;

  constructor(options: ComputeBrokerOptions) {
    this.nodeId = options.nodeId;
    this.ledger = new LeaseLedger(options.storage);
    this.driver = options.driver;
    this.now = options.now ?? (() => Date.now());
    this.maxTtlMs = options.maxTtlMs ?? DEFAULT_MAX_TTL_MS;
    this.onExpire = options.onExpire;
  }

  /**
   * Acquire a compute sandbox lease (OECP acquire).
   * Idempotent: identical submitter and name returns existing lease or restarts if stopped.
   */
  async acquire(submitter: string, spec: AcquireSpec): Promise<SandboxDescriptor> {
    if (!spec.name || typeof spec.name !== 'string') {
      throw new OnlyneError('invalid_request', 'Acquire requires a non-empty name.');
    }
    if (!spec.container?.image || typeof spec.container.image !== 'string') {
      throw new OnlyneError('invalid_request', 'Acquire requires container.image.');
    }

    const activeWithName = await this.ledger.findActiveByName(spec.name);
    if (activeWithName && activeWithName.submitter !== submitter) {
      throw new OnlyneError('forbidden', `Lease "${spec.name}" is owned by another principal.`);
    }

    const requestedTtl = spec.lease?.ttl_ms ?? DEFAULT_TTL_MS;
    if (requestedTtl <= 0) {
      throw new OnlyneError('invalid_request', 'ttl_ms must be positive.');
    }
    const effectiveMaxTtl = spec.lease?.max_ttl_ms !== undefined
      ? Math.min(spec.lease.max_ttl_ms, this.maxTtlMs)
      : this.maxTtlMs;
    if (requestedTtl > effectiveMaxTtl) {
      throw new OnlyneError('capacity_exceeded', `Requested ttl_ms (${requestedTtl}) exceeds max_ttl_ms (${effectiveMaxTtl}).`);
    }

    const existing = await this.ledger.findByName(submitter, spec.name);
    const currentTime = this.now();

    if (existing) {
      if (existing.state === 'running') {
        return existing.descriptor!;
      }
      if (existing.state === 'stopped') {
        // Re-start container
        if (existing.container_id) {
          const freshConn = await this.driver.start(existing.container_id);
          const ttl_ms = requestedTtl;
          const retention_ms = spec.lease?.retention_ms ?? (existing.spec.lease?.retention_ms ?? DEFAULT_RETENTION_MS);
          const expires_at = currentTime + ttl_ms;
          const retention_until = expires_at + retention_ms;

          existing.state = 'running';
          existing.expires_at = expires_at;
          existing.retention_until = retention_until;
          existing.descriptor!.state = 'running';
          existing.descriptor!.expires_at = expires_at;
          existing.descriptor!.retention_until = retention_until;
          if (freshConn) existing.descriptor!.connection = freshConn;

          await this.ledger.save(existing);
          return existing.descriptor!;
        }
      }
      // If destroyed or failed, we allow re-provisioning with a fresh lease ID
    }

    const lease_id = `cls_${crypto.randomUUID().replace(/-/g, '').slice(0, 16)}`;
    const ttl_ms = requestedTtl;
    const max_ttl_ms = effectiveMaxTtl;
    const retention_ms = spec.lease?.retention_ms ?? DEFAULT_RETENTION_MS;
    const expires_at = currentTime + ttl_ms;
    const retention_until = expires_at + retention_ms;

    const alloc = await this.driver.provision(lease_id, spec);

    const descriptor: SandboxDescriptor = {
      lease_id,
      name: spec.name,
      resource_ref: { owner: this.nodeId, id: lease_id },
      state: 'running',
      actual_provider: alloc.provider,
      created_at: currentTime,
      expires_at,
      retention_until,
      connection: alloc.connection,
      resolved_env: alloc.resolved_env,
      allocated: {
        arch: alloc.arch,
        container_id: alloc.container_id,
        host_ip: alloc.host_ip,
      },
    };

    const record: LeaseRecord = {
      lease_id,
      name: spec.name,
      submitter,
      spec,
      state: 'running',
      provider: alloc.provider,
      container_id: alloc.container_id,
      created_at: currentTime,
      total_ttl_ms: ttl_ms,
      max_ttl_ms,
      expires_at,
      retention_until,
      descriptor,
    };

    await this.ledger.save(record);
    return descriptor;
  }

  /**
   * Renew lease TTL (OECP renew).
   */
  async renew(submitter: string, lease_id: string, ttl_ms: number): Promise<SandboxDescriptor> {
    if (ttl_ms <= 0) throw new OnlyneError('invalid_request', 'ttl_ms must be positive.');
    const record = await this.ledger.get(lease_id);
    if (!record) throw new OnlyneError('not_found', `Lease ${lease_id} not found.`);
    if (record.submitter !== submitter) throw new OnlyneError('forbidden', 'Caller is not the lease owner.');
    if (record.state !== 'running') {
      throw new OnlyneError('conflict', `Cannot renew lease in state ${record.state}.`);
    }

    if (record.total_ttl_ms + ttl_ms > record.max_ttl_ms) {
      throw new OnlyneError('capacity_exceeded', `Renewal exceeds max_ttl_ms limit of ${record.max_ttl_ms}.`);
    }

    const currentTime = this.now();
    record.total_ttl_ms += ttl_ms;
    record.expires_at = Math.max(record.expires_at, currentTime) + ttl_ms;
    record.retention_until = record.expires_at + (record.spec.lease?.retention_ms ?? DEFAULT_RETENTION_MS);
    record.descriptor!.expires_at = record.expires_at;
    record.descriptor!.retention_until = record.retention_until;

    await this.ledger.save(record);
    return record.descriptor!;
  }

  /**
   * Release sandbox to stopped state, keeping storage volumes intact (OECP release).
   */
  async release(submitter: string, lease_id: string): Promise<SandboxDescriptor> {
    const record = await this.ledger.get(lease_id);
    if (!record) throw new OnlyneError('not_found', `Lease ${lease_id} not found.`);
    if (record.submitter !== submitter) throw new OnlyneError('forbidden', 'Caller is not the lease owner.');

    if (record.state === 'running' && record.container_id) {
      await this.driver.stop(record.container_id);
      record.state = 'stopped';
      record.descriptor!.state = 'stopped';
      await this.ledger.save(record);
    }

    return record.descriptor!;
  }

  /**
   * Physically destroy sandbox container and clean up temporary volumes (OECP destroy).
   */
  async destroy(submitter: string, lease_id: string): Promise<SandboxDescriptor> {
    const record = await this.ledger.get(lease_id);
    if (!record) throw new OnlyneError('not_found', `Lease ${lease_id} not found.`);
    if (record.submitter !== submitter) throw new OnlyneError('forbidden', 'Caller is not the lease owner.');

    if (record.state !== 'destroyed' && record.container_id) {
      await this.driver.destroy(record.container_id);
      record.state = 'destroyed';
      record.descriptor!.state = 'destroyed';
      await this.ledger.save(record);
    }

    return record.descriptor!;
  }

  /**
   * Inspect current lease or list submitter's leases (OECP status).
   */
  async status(submitter: string, lease_id?: string): Promise<SandboxDescriptor | SandboxDescriptor[]> {
    if (lease_id) {
      const record = await this.ledger.get(lease_id);
      if (!record) throw new OnlyneError('not_found', `Lease ${lease_id} not found.`);
      if (record.submitter !== submitter) throw new OnlyneError('forbidden', 'Caller is not the lease owner.');
      return record.descriptor!;
    }

    const records = await this.ledger.listBySubmitter(submitter);
    return records.map(r => r.descriptor!).filter(Boolean);
  }

  /**
   * Safety Net Sweep: background timer check.
   * - Running leases past expires_at -> Stopped
   * - Stopped leases past retention_until -> Destroyed
   */
  async sweep(): Promise<{ stopped: string[]; destroyed: string[] }> {
    const records = await this.ledger.all();
    const currentTime = this.now();
    const stopped: string[] = [];
    const destroyed: string[] = [];

    for (const record of records) {
      if (record.state === 'running' && currentTime >= record.expires_at) {
        if (record.container_id) {
          try { await this.driver.stop(record.container_id); } catch { /* ignore */ }
        }
        record.state = 'stopped';
        record.descriptor!.state = 'stopped';
        await this.ledger.save(record);
        stopped.push(record.lease_id);
        if (this.onExpire) {
          try { await this.onExpire(record); } catch { /* ignore */ }
        }
      } else if (record.state === 'stopped' && currentTime >= record.retention_until) {
        if (record.container_id) {
          try { await this.driver.destroy(record.container_id); } catch { /* ignore */ }
        }
        record.state = 'destroyed';
        record.descriptor!.state = 'destroyed';
        await this.ledger.save(record);
        destroyed.push(record.lease_id);
      }
    }

    return { stopped, destroyed };
  }
}
