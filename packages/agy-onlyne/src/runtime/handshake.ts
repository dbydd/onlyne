import {
	OnlyneError,
	trustPhase,
	type KeyExchange,
	type Message,
	type NodeId,
	type OnlyneNode,
	type Storage,
	type ThreadRef,
	type TrustEntry,
	type TrustPhase,
} from "@onlyne/lib";
import { evaluateAcquisition, type AcquireInput, type AcquireVerdict, type AcquisitionPolicy } from "@onlyne/node-host/acquire";
import type { AcquisitionAuditEntry } from "../core/types.ts";

export const TRUST_THREAD = "trust";

export function trustThread(nodeId: NodeId): ThreadRef {
	return { owner: nodeId, id: TRUST_THREAD };
}

export function isTrustThread(nodeId: NodeId, ref: ThreadRef | undefined): boolean {
	return ref !== undefined && ref.owner === nodeId && ref.id === TRUST_THREAD;
}

export interface StatementObservation {
	observed?: string;
	at: string;
	cause: string;
}

export interface HandshakeFacts {
	node_id: NodeId;
	fingerprint: string;
	phase: TrustPhase;
	pending: boolean;
}

const OBSERVED = "acquire/observed/";
const DELIVERED = "acquire/delivered/";
const AUDIT = "acquire/audit/";
const MAX_AUDIT = 200;

function observedKey(nodeId: NodeId, fingerprint: string): string {
	return `${OBSERVED}${encodeURIComponent(nodeId)}/${encodeURIComponent(fingerprint)}`;
}

function deliveredKey(nodeId: NodeId, fingerprint: string, phase: TrustPhase): string {
	return `${DELIVERED}${encodeURIComponent(nodeId)}/${encodeURIComponent(fingerprint)}/${phase}`;
}

export function handshakeFacts(meta: Message["meta"] | undefined): HandshakeFacts | undefined {
	const raw = meta?.data?.handshake;
	if (raw === null || raw === undefined || typeof raw !== "object" || Array.isArray(raw)) return undefined;
	const rec = raw as Record<string, unknown>;
	if (typeof rec.node_id !== "string" || typeof rec.fingerprint !== "string" || typeof rec.phase !== "string") return undefined;
	if (!["invited", "requested", "exchanged", "revoked"].includes(rec.phase)) return undefined;
	return {
		node_id: rec.node_id,
		fingerprint: rec.fingerprint,
		phase: rec.phase as TrustPhase,
		pending: rec.pending === true,
	};
}

export class AcquisitionLog {
	constructor(private readonly storage: Storage) {}

	async observe(nodeId: NodeId, fingerprint: string, observation: StatementObservation): Promise<void> {
		await this.storage.transaction(tx => tx.put(observedKey(nodeId, fingerprint), observation));
	}

	async transition(nodeId: NodeId, fingerprint: string, at: string, cause: string): Promise<void> {
		await this.storage.transaction(async tx => {
			const key = observedKey(nodeId, fingerprint);
			const current = await tx.get<StatementObservation>(key);
			await tx.put(key, {
				...(current?.observed === undefined ? {} : { observed: current.observed }),
				at,
				cause,
			});
		});
	}

	async observation(nodeId: NodeId, fingerprint: string): Promise<StatementObservation | undefined> {
		return this.storage.transaction(tx => tx.get<StatementObservation>(observedKey(nodeId, fingerprint)));
	}

	async delivered(nodeId: NodeId, fingerprint: string, phase: TrustPhase): Promise<boolean> {
		return (await this.storage.transaction(tx => tx.get(deliveredKey(nodeId, fingerprint, phase)))) !== undefined;
	}

	async markDelivered(nodeId: NodeId, fingerprint: string, phase: TrustPhase, at: string): Promise<void> {
		await this.storage.transaction(tx => tx.put(deliveredKey(nodeId, fingerprint, phase), {
			node_id: nodeId,
			fingerprint,
			phase,
			at,
		}));
	}

	async settle(nodeId: NodeId, fingerprint?: string): Promise<void> {
		const prefix = `${DELIVERED}${encodeURIComponent(nodeId)}/${fingerprint === undefined ? "" : `${encodeURIComponent(fingerprint)}/`}`;
		await this.storage.transaction(async tx => {
			for (const row of await tx.scan(prefix)) await tx.delete(row.key);
		});
	}

	async recordAudit(entry: AcquisitionAuditEntry): Promise<void> {
		const key = `${AUDIT}${entry.at}-${crypto.randomUUID().slice(0, 8)}`;
		await this.storage.transaction(async tx => {
			await tx.put(key, entry);
			const rows = await tx.scan<AcquisitionAuditEntry>(AUDIT);
			if (rows.length > MAX_AUDIT) {
				const dropCount = rows.length - MAX_AUDIT;
				rows.sort((a, b) => a.key.localeCompare(b.key));
				for (let i = 0; i < dropCount; i++) {
					await tx.delete(rows[i]!.key);
				}
			}
		});
	}

	async listAudit(limit = 50): Promise<AcquisitionAuditEntry[]> {
		const rows = await this.storage.transaction(tx => tx.scan<AcquisitionAuditEntry>(AUDIT));
		return rows
			.map(row => row.value)
			.sort((a, b) => b.at.localeCompare(a.at))
			.slice(0, limit);
	}
}

export interface ApprovalDeps {
	node: OnlyneNode;
	policy: AcquisitionPolicy;
	log: AcquisitionLog;
	recordedAddresses(nodeId: NodeId): Promise<string[]>;
}

export interface ApprovalJudgement {
	verdict: AcquireVerdict;
	input: AcquireInput;
}

export async function judgeAcquisition(
	deps: ApprovalDeps,
	nodeId: NodeId,
	fingerprint: string,
	declared?: string,
): Promise<ApprovalJudgement> {
	const [obs, recorded] = await Promise.all([
		deps.log.observation(nodeId, fingerprint),
		deps.recordedAddresses(nodeId),
	]);
	const input: AcquireInput = {
		...(obs?.observed === undefined ? {} : { observed: obs.observed }),
		...(declared === undefined ? {} : { declared }),
		recorded,
	};
	const verdict = await evaluateAcquisition(deps.policy, input);
	return { verdict, input };
}

export async function appendHandshakeThread(
	node: OnlyneNode,
	entry: TrustEntry,
	cause: string,
	summary: string,
): Promise<void> {
	const ref = trustThread(node.auth.actor);
	const existing = await node.threads.list(node.auth, { page_size: 100 });
	if (!existing.items.some(t => t.thread_ref.id === TRUST_THREAD && t.thread_ref.owner === node.auth.actor)) {
		await node.threads.create(node.auth, {
			id: TRUST_THREAD,
			title: "Trust & Key Exchanges",
			idempotency_key: `init-${TRUST_THREAD}`,
			retry_until: new Date(Date.now() + 300_000).toISOString(),
		}).catch(() => undefined);
	}
	const phase = trustPhase(entry);
	const message: Message = {
		id: `trust-${entry.node_id}-${phase}-${Date.now()}-${crypto.randomUUID().slice(0, 6)}`,
		sender: node.auth.actor,
		delivery_kind: "notify",
		msg: summary,
		meta: {
			thread: ref,
			data: {
				handshake: {
					node_id: entry.node_id,
					fingerprint: entry.fingerprint,
					phase,
					pending: phase === "invited",
					cause,
				},
			},
		},
	};
	await node.messages.admit(node.auth, {
		idempotency_key: `admit-${message.id}`,
		retry_until: new Date(Date.now() + 300_000).toISOString(),
		target_endpoint: node.auth.actor,
		message,
		plan: { response: "none", execution: "existing" },
	}).catch(() => undefined);
}

export async function beginPeer(
	deps: ApprovalDeps,
	nodeId: NodeId,
	actor: "human" | "agent" = "human",
): Promise<{ exchange: KeyExchange; verdict: AcquireVerdict; confirmed?: TrustEntry }> {
	const exchange = await deps.node.trust.begin(deps.node.auth, nodeId);
	const now = new Date().toISOString();
	await deps.log.observe(exchange.node_id, exchange.fingerprint, {
		observed: exchange.base_url,
		at: now,
		cause: "begin",
	});
	const { verdict, input } = await judgeAcquisition(deps, exchange.node_id, exchange.fingerprint, exchange.base_url);
	if (actor === "human" || verdict.allowed) {
		const confirmed = await exchange.confirm(exchange.fingerprint);
		await deps.log.recordAudit({
			at: now,
			actor,
			action: "trust_begin_confirm",
			node_id: exchange.node_id,
			fingerprint: exchange.fingerprint,
			observed: input.observed,
			declared: input.declared,
			policy: deps.policy.policy,
			allowed: verdict.allowed,
			reason: verdict.reason,
			override: actor === "human" && !verdict.allowed,
		});
		return { exchange, verdict, confirmed };
	}
	await deps.log.recordAudit({
		at: now,
		actor: "policy",
		action: "trust_begin_held",
		node_id: exchange.node_id,
		fingerprint: exchange.fingerprint,
		observed: input.observed,
		declared: input.declared,
		policy: deps.policy.policy,
		allowed: false,
		reason: verdict.reason,
	});
	throw new OnlyneError("forbidden", `Acquisition policy refused auto-approval for ${nodeId} (${verdict.reason})`);
}
