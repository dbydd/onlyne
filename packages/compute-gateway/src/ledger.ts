import { OnlyneError, type Storage } from '@onlyne/lib';
import type { AcquireSpec, LeaseRecord, LeaseState, SandboxDescriptor } from '@onlyne/compute-core';
import type { SandboxDriver } from '@onlyne/compute-core';

const LEASE_PREFIX = 'compute/lease/';
const NAME_INDEX_PREFIX = 'compute/name/';

function leaseKey(id: string): string {
  return `${LEASE_PREFIX}${encodeURIComponent(id)}`;
}

function nameIndexKey(submitter: string, name: string): string {
  return `${NAME_INDEX_PREFIX}${encodeURIComponent(submitter)}/${encodeURIComponent(name)}`;
}

export class LeaseLedger {
  constructor(private readonly storage: Storage) {}

  async get(leaseId: string): Promise<LeaseRecord | undefined> {
    return this.storage.transaction(async tx => tx.get<LeaseRecord>(leaseKey(leaseId)));
  }

  async findByName(submitter: string, name: string): Promise<LeaseRecord | undefined> {
    return this.storage.transaction(async tx => {
      const leaseId = await tx.get<string>(nameIndexKey(submitter, name));
      if (!leaseId) return undefined;
      return tx.get<LeaseRecord>(leaseKey(leaseId));
    });
  }

  async findActiveByName(name: string): Promise<LeaseRecord | undefined> {
    return this.storage.transaction(async tx => {
      const scan = await tx.scan<LeaseRecord>(LEASE_PREFIX);
      return scan
        .map(row => row.value)
        .find(rec => rec.name === name && (rec.state === 'running' || rec.state === 'stopped' || rec.state === 'provisioning'));
    });
  }

  async save(record: LeaseRecord): Promise<void> {
    await this.storage.transaction(async tx => {
      await tx.put(leaseKey(record.lease_id), record);
      await tx.put(nameIndexKey(record.submitter, record.name), record.lease_id);
    });
  }

  async listBySubmitter(submitter?: string): Promise<LeaseRecord[]> {
    return this.storage.transaction(async tx => {
      const scan = await tx.scan<LeaseRecord>(LEASE_PREFIX);
      return scan
        .map(row => row.value)
        .filter(rec => submitter === undefined || rec.submitter === submitter)
        .sort((a, b) => b.created_at - a.created_at);
    });
  }

  async all(): Promise<LeaseRecord[]> {
    return this.storage.transaction(async tx => {
      const scan = await tx.scan<LeaseRecord>(LEASE_PREFIX);
      return scan.map(row => row.value);
    });
  }
}
