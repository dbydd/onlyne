import { Database } from "bun:sqlite";
import { chmodSync, existsSync, mkdirSync } from "node:fs";
import { dirname } from "node:path";
import type { Entry, Storage, ThreadRef, Transaction } from "@onlyne/lib";
import type { ContextRecord } from "./types.ts";

/** Key/value rows under one SQLite file; the library's durable state and the plugin's registry share it. */
export class SqliteStorage implements Storage {
	readonly guarantee = "durable" as const;
	readonly #db: Database;
	#tail: Promise<unknown> = Promise.resolve();
	#closed = false;

	constructor(path: string) {
		// The store carries trust rows and, while a rotation is unacknowledged, the old private
		// key that proves it. It is secret material: 0700 for the directory, 0600 for the files,
		// including the WAL and shared-memory sidecars SQLite creates beside them.
		const dir = dirname(path);
		mkdirSync(dir, { recursive: true, mode: 0o700 });
		chmodSync(dir, 0o700);
		this.#db = new Database(path, { create: true });
		this.#db.exec("PRAGMA journal_mode = WAL");
		this.#db.exec("PRAGMA synchronous = FULL");
		this.#db.exec("PRAGMA busy_timeout = 5000");
		this.#db.exec("CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL)");
		for (const file of [path, `${path}-wal`, `${path}-shm`]) {
			if (existsSync(file)) chmodSync(file, 0o600);
		}
	}

	/** Serialize every transaction; SQLite's own BEGIN IMMEDIATE would otherwise race across awaits. */
	transaction<T>(work: (tx: Transaction) => Promise<T>): Promise<T> {
		const run = () => this.#execute(work);
		const result = this.#tail.then(run, run);
		this.#tail = result.then(
			() => undefined,
			() => undefined,
		);
		return result;
	}

	async #execute<T>(work: (tx: Transaction) => Promise<T>): Promise<T> {
		if (this.#closed) throw new Error("The Onlyne store is closed.");
		this.#db.exec("BEGIN IMMEDIATE");
		const tx: Transaction = {
			get: async <V>(key: string) => this.#get<V>(key),
			put: async <V>(key: string, value: V) => this.#put(key, value),
			delete: async (key: string) => {
				this.#db.query("DELETE FROM kv WHERE key = ?").run(key);
			},
			scan: async <V>(prefix: string) => this.#scan<V>(prefix),
		};
		try {
			const value = await work(tx);
			this.#db.exec("COMMIT");
			return value;
		} catch (error) {
			try {
				this.#db.exec("ROLLBACK");
			} catch {
				// The transaction may already be gone; preserve the original failure.
			}
			throw error;
		}
	}

	#get<T>(key: string): T | undefined {
		const row = this.#db.query<{ value: string }, [string]>("SELECT value FROM kv WHERE key = ?").get(key);
		return row === null ? undefined : (JSON.parse(row.value) as T);
	}

	#put(key: string, value: unknown): void {
		this.#db
			.query("INSERT INTO kv (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
			.run(key, JSON.stringify(value));
	}

	#scan<T>(prefix: string): Array<{ key: string; value: T }> {
		const rows = this.#db
			.query<{ key: string; value: string }, [number, string]>(
				"SELECT key, value FROM kv WHERE substr(key, 1, ?) = ? ORDER BY key",
			)
			.all(prefix.length, prefix);
		return rows.map(row => ({ key: row.key, value: JSON.parse(row.value) as T }));
	}

	close(): void {
		if (this.#closed) return;
		this.#closed = true;
		this.#db.close();
	}
}

const COUNTER_KEY = "meta/context-counter";
const CONTEXT_PREFIX = "context/";

/**
 * Whether a registered context can answer a delivery. Only the session holding this node's
 * own send right can: a subagent context reads and drafts, never sends. Advertising one as a
 * continuation target hands the sender a message nobody there can reply to, and the sender
 * has no way to see that from the alias (caerulea report 4, problem 7).
 */
export function canAnswer(record: ContextRecord): boolean {
	return record.agent_id !== undefined && record.agent_id === record.main_agent_id;
}

/** Alias allocation and binding for execution contexts; the store is the authority across restarts. */
export class ContextRegistry {
	constructor(private readonly storage: Storage, private readonly nodeId: string, private readonly mainAgentId: string) {}

	/** The main OMP session is addressable like a registered execution context. */
	async ensureMain(): Promise<ContextRecord> {
		return this.storage.transaction(async tx => {
			const key = `${CONTEXT_PREFIX}main`;
			const current = await tx.get<ContextRecord>(key);
			if (current !== undefined) return current;
			const record: ContextRecord = {
				alias: "main", node_id: this.nodeId, main_agent_id: this.mainAgentId, agent_id: this.mainAgentId,
				summary: "Main OMP session", state: "idle", created_at: new Date().toISOString(),
			};
			await tx.put(key, record);
			return record;
		});
	}

	async allocate(thread_ref: ThreadRef | undefined, summary: string, source?: Entry["message"]): Promise<ContextRecord> {
		return this.storage.transaction(async tx => {
			const counter = (await tx.get<number>(COUNTER_KEY)) ?? 0;
			const next = counter + 1;
			await tx.put(COUNTER_KEY, next);
			const alias = `c-${String(next)}`;
			const record: ContextRecord = {
				alias,
				node_id: this.nodeId,
				main_agent_id: this.mainAgentId,
				...(source === undefined ? {} : { source: { sender: source.sender, id: source.id } }),
				...(thread_ref === undefined ? {} : { thread_ref }),
				summary,
				state: "unbound",
				created_at: new Date().toISOString(),
			};
			await tx.put(`${CONTEXT_PREFIX}${alias}`, record);
			return record;
		});
	}

	async bind(
		alias: string,
		binding: { agent_id: string; session_file?: string; parent_id?: string },
	): Promise<ContextRecord> {
		return this.storage.transaction(async tx => {
			const key = `${CONTEXT_PREFIX}${alias}`;
			const current = await tx.get<ContextRecord>(key);
			if (current === undefined) throw new Error(`No execution context is registered as ${alias}.`);
			const next: ContextRecord = {
				...current,
				agent_id: binding.agent_id,
				...(binding.session_file === undefined ? {} : { session_file: binding.session_file }),
				...(binding.parent_id === undefined ? {} : { parent_id: binding.parent_id }),
				state: current.agent_id === binding.agent_id ? current.state : "idle",
				bound_at: new Date().toISOString(),
			};
			await tx.put(key, next);
			return next;
		});
	}

	async find(alias: string): Promise<ContextRecord | undefined> {
		return this.storage.transaction(tx => tx.get<ContextRecord>(`${CONTEXT_PREFIX}${alias}`));
	}

	async list(): Promise<ContextRecord[]> {
		return this.storage.transaction(async tx =>
			(await tx.scan<ContextRecord>(CONTEXT_PREFIX)).map(row => row.value),
		);
	}

	async updateState(alias: string, state: ContextRecord["state"]): Promise<ContextRecord | undefined> {
		return this.storage.transaction(async tx => {
			const key = `${CONTEXT_PREFIX}${alias}`;
			const current = await tx.get<ContextRecord>(key);
			if (current === undefined) return undefined;
			const next = { ...current, state };
			await tx.put(key, next);
			return next;
		});
	}
}
