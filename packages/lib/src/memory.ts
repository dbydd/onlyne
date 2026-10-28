import type { Storage, Transaction } from './types.ts';

/** Process-local storage for embedded ephemeral nodes. It does not claim durability. */
export class MemoryStorage implements Storage {
  readonly guarantee = 'binding-defined' as const;
  private records = new Map<string, unknown>();
  private pending: Promise<void> = Promise.resolve();

  async transaction<T>(work: (tx: Transaction) => Promise<T>): Promise<T> {
    const before = this.pending;
    let release!: () => void;
    this.pending = new Promise<void>(resolve => { release = resolve; });
    await before;
    const changes = new Map<string, unknown>();
    const deleted = new Set<string>();
    const tx: Transaction = {
      get: async <V>(key: string) => structuredClone(deleted.has(key) ? undefined : changes.has(key) ? changes.get(key) : this.records.get(key)) as V | undefined,
      put: async <V>(key: string, value: V) => { changes.set(key, structuredClone(value)); deleted.delete(key); },
      delete: async (key: string) => { changes.delete(key); deleted.add(key); },
      scan: async <V>(prefix: string) => {
        const records = new Map(this.records);
        deleted.forEach(key => records.delete(key));
        changes.forEach((value, key) => records.set(key, value));
        return Array.from(records).filter(([key]) => key.startsWith(prefix)).map(([key, value]) => ({ key, value: structuredClone(value) as V }));
      },
    };
    try {
      const result = await work(tx);
      deleted.forEach(key => this.records.delete(key));
      changes.forEach((value, key) => this.records.set(key, value));
      return structuredClone(result);
    } finally { release(); }
  }
}
