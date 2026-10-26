import type { Storage, Transaction } from '@onlyne/lib';
import { mkdirSync, renameSync, unlinkSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname } from 'node:path';

interface Snapshot { [key: string]: unknown }

/**
 * The disk Storage: the whole store is one JSON file. A transaction buffers its writes and
 * the commit is one atomic rename, so a crash mid-write leaves the previous snapshot whole.
 * All transaction work is serialized: one writer at a time, full-snapshot commit.
 */
export class FileStorage implements Storage {
  readonly guarantee = 'binding-defined' as const;
  private records: Snapshot;
  private pending: Promise<void> = Promise.resolve();
  private readonly path: string;

  constructor(path: string) {
    this.path = path;
    let loaded: Snapshot = {};
    try {
      loaded = JSON.parse(readFileSync(path, 'utf8')) as Snapshot;
    } catch (error) {
      if ((error as { code?: string }).code !== 'ENOENT') throw error;
    }
    if (loaded === null || typeof loaded !== 'object' || Array.isArray(loaded)) {
      throw new Error(`The store at ${path} is not a key-value snapshot.`);
    }
    this.records = loaded;
  }

  async transaction<T>(work: (tx: Transaction) => Promise<T>): Promise<T> {
    const before = this.pending;
    const { promise, resolve } = Promise.withResolvers<void>();
    this.pending = promise;
    await before;
    const changes = new Map<string, unknown>();
    const deleted = new Set<string>();
    const tx: Transaction = {
      get: async <V>(key: string) => (deleted.has(key) ? undefined : changes.has(key) ? changes.get(key) : this.records[key]) as V | undefined,
      put: async <V>(key: string, value: V) => { changes.set(key, value); deleted.delete(key); },
      delete: async (key: string) => { changes.delete(key); deleted.add(key); },
      scan: async <V>(prefix: string) => {
        const merged: Snapshot = { ...this.records };
        deleted.forEach(key => delete merged[key]);
        changes.forEach((value, key) => { merged[key] = value; });
        return Object.entries(merged)
          .filter(([key]) => key.startsWith(prefix))
          .map(([key, value]) => ({ key, value: structuredClone(value) as V }));
      },
    };
    try {
      const result = await work(tx);
      let touched = false;
      deleted.forEach(key => { delete this.records[key]; touched = true; });
      changes.forEach((value, key) => { this.records[key] = structuredClone(value); touched = true; });
      if (touched) this.commit();
      return structuredClone(result);
    } finally { resolve(); }
  }

  /** Write temp + rename: a reader either sees the previous file whole or the new one whole. */
  private commit(): void {
    const tmp = `${this.path}.tmp-${process.pid}`;
    try {
      mkdirSync(dirname(this.path), { recursive: true });
      writeFileSync(tmp, JSON.stringify(this.records));
      renameSync(tmp, this.path);
    } catch (error) {
      try { unlinkSync(tmp); } catch { /* nothing to clean */ }
      throw error;
    }
  }
}
