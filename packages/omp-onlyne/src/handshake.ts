import { OnlyneError, type Json, type Message, type NodeId, type Storage, type ThreadRef, type TrustEntry, type TrustPhase } from "@onlyne/lib";
import { evaluateAcquisition, type AcquireInput, type AcquireVerdict, type AcquisitionPolicy } from "./acquire.ts";
import { bridgeText as t } from "./bridge-text.ts";
import { hostText } from "./host-text.ts";

/**
 * RFC 0018 §3.2: the handshake entries live in one thread this node owns alone, so nothing in
 * them ever leaves the node, and the business thread tree stays a record of conversations
 * rather than a log of system events.
 */
export const TRUST_THREAD = "trust";

export function trustThread(nodeId: NodeId): ThreadRef {
	return { owner: nodeId, id: TRUST_THREAD };
}

export function isTrustThread(nodeId: NodeId, ref: ThreadRef | undefined): boolean {
	return ref !== undefined && ref.owner === nodeId && ref.id === TRUST_THREAD;
}

/** RFC 0018 §2.1: where one peer statement's connection arrived from. */
export interface StatementObservation {
	/** The address the connection came from; absent when this host could not observe one. */
	observed?: string;
	at: string;
	cause: string;
}

const OBSERVED = "acquire/observed/";
const DELIVERED = "acquire/delivered/";

function observedKey(nodeId: NodeId, fingerprint: string): string {
	return `${OBSERVED}${encodeURIComponent(nodeId)}/${encodeURIComponent(fingerprint)}`;
}

function deliveredKey(nodeId: NodeId, fingerprint: string, phase: TrustPhase): string {
	return `${DELIVERED}${encodeURIComponent(nodeId)}/${encodeURIComponent(fingerprint)}/${phase}`;
}

/**
 * RFC 0018 §2.1/§3: the two durable records this host keeps about a handshake. The observed
 * address is written for every statement, before any verdict, so an approval decided later
 * reads the address that statement itself arrived from and never the address of the call that
 * asks (§2.1). The delivered records are one per `(node_id, fingerprint, phase)`, so a
 * replayed event appends no second entry.
 */
export class AcquisitionLog {
	constructor(private readonly storage: Storage) {}

	async observe(nodeId: NodeId, fingerprint: string, observation: StatementObservation): Promise<void> {
		await this.storage.transaction(tx => tx.put(observedKey(nodeId, fingerprint), observation));
	}

	/**
	 * A local transition is still an observed trust event, but it must not replace the address
	 * of the peer statement. Keep its timestamp and cause beside the original address instead.
	 */
	async transition(nodeId: NodeId, fingerprint: string, at: string, cause: string): Promise<void> {
		await this.storage.transaction(async tx => {
			const key = observedKey(nodeId, fingerprint);
			const current = await tx.get<StatementObservation>(key);
			await tx.put(key, { ...(current?.observed === undefined ? {} : { observed: current.observed }), at, cause });
		});
	}

	async observation(nodeId: NodeId, fingerprint: string): Promise<StatementObservation | undefined> {
		return this.storage.transaction(tx => tx.get<StatementObservation>(observedKey(nodeId, fingerprint)));
	}

	async delivered(nodeId: NodeId, fingerprint: string, phase: TrustPhase): Promise<boolean> {
		return (await this.storage.transaction(tx => tx.get(deliveredKey(nodeId, fingerprint, phase)))) !== undefined;
	}

	async markDelivered(nodeId: NodeId, fingerprint: string, phase: TrustPhase, at: string): Promise<void> {
		await this.storage.transaction(tx => tx.put(deliveredKey(nodeId, fingerprint, phase), { node_id: nodeId, fingerprint, phase, at }));
	}

	/**
	 * RFC 0018 §3: the reports an agent already acted on are settled. A revocation names no
	 * fingerprint, so it settles every report held for the node; a later statement opens its own.
	 */
	async settle(nodeId: NodeId, fingerprint?: string): Promise<void> {
		const prefix = `${DELIVERED}${encodeURIComponent(nodeId)}/${fingerprint === undefined ? "" : `${encodeURIComponent(fingerprint)}/`}`;
		await this.storage.transaction(async tx => {
			for (const row of await tx.scan(prefix)) await tx.delete(row.key);
		});
	}
}

/** The machine facts a handshake entry carries beside its body (RFC 0018 §3). */
export interface HandshakeFacts {
	node_id: NodeId;
	fingerprint: string;
	phase: TrustPhase;
	/** Whether this entry owes an agent decision; a status update owes none. */
	pending: boolean;
}

/** The facts one handshake entry carries, or undefined for an entry that is not one. */
export function handshakeFacts(meta: Message["meta"] | undefined): HandshakeFacts | undefined {
	const value = meta?.data?.trust;
	if (value === null || value === undefined || typeof value !== "object" || Array.isArray(value)) return undefined;
	const row = value as Record<string, Json>;
	const { node_id: node, fingerprint, phase, pending } = row;
	if (typeof node !== "string" || typeof fingerprint !== "string" || typeof pending !== "boolean") return undefined;
	if (phase !== "invited" && phase !== "requested" && phase !== "exchanged" && phase !== "revoked") return undefined;
	return { node_id: node, fingerprint, phase, pending };
}

export interface HandshakeInput {
	node_id: NodeId;
	fingerprint: string;
	phase: TrustPhase;
	/** The address the peer wrote into its own statement. */
	declared: string;
	/** The address that statement arrived from; absent when this host observed none. */
	observed?: string;
	policy: AcquisitionPolicy;
	/** The judgement made for this event, when one was made. */
	judgement?: ApprovalJudgement;
	/** Whether an approval was committed without an agent deciding first. */
	approved: boolean;
}

/**
 * RFC 0018 §3: one handshake entry. The body carries what the judgement read and concluded;
 * the action identifiers appear only when a decision is actually owed, and a status update
 * says so instead. `pending` is the same fact for the host's todo hook.
 */
export function handshakeEntry(input: HandshakeInput): { msg: string; trust: { [key: string]: Json } } {
	// RFC 0018 §3: only an invitation this host did not approve itself is work. A pending local
	// approval waits on the peer, so there is nothing to do and nothing to wake a turn for.
	const pending = input.phase === "invited" && !input.approved;
	const lines = [
		t("trustHeader", { phase: input.phase, node: input.node_id }),
		t("trustFingerprint", { fingerprint: input.fingerprint }),
		t("trustDeclared", { declared: input.declared }),
		t("trustObserved", { observed: input.observed ?? t("trustUnobserved") }),
		t("trustPolicy", { policy: input.policy.policy }),
	];
	if (input.judgement !== undefined) {
		lines.push(t("trustJudgement", {
			reason: input.judgement.verdict.reason,
			conclusion: input.judgement.verdict.allowed ? t("trustAllowed") : t("trustRefused"),
		}));
	}
	if (input.phase === "invited") lines.push(input.approved ? t("trustApproved") : t("trustInvited"));
	else if (input.phase === "requested") lines.push(t("trustRequested"));
	else if (input.phase === "exchanged") lines.push(t("trustExchanged"));
	else lines.push(t("trustRevoked"));
	if (input.phase === "invited" && !input.approved) {
		// RFC 0018 §3: under `none` the agent may only look. Offering an action the policy
		// refuses would ask for a decision that can never be made.
		lines.push(input.policy.policy === "none"
			? t("trustPanelOnly")
			: t("trustAcceptAction", { node: input.node_id, fingerprint: input.fingerprint }));
	}
	if (pending) lines.push(t("trustRevokeAction", { node: input.node_id }));
	return { msg: lines.join("\n"), trust: { node_id: input.node_id, fingerprint: input.fingerprint, phase: input.phase, pending } };
}

/** RFC 0018 §2: one approval judgement and every input it read. */
export interface ApprovalJudgement {
	at: string;
	node_id: NodeId;
	fingerprint: string;
	policy: AcquisitionPolicy;
	input: AcquireInput;
	verdict: AcquireVerdict;
}

/** The judgement as the audit line records it: the time, the peer, and every input the verdict read. */
export function judgementFacts(judgement: ApprovalJudgement): Record<string, unknown> {
	const { input, verdict } = judgement;
	return {
		at: judgement.at,
		node: judgement.node_id,
		fingerprint: judgement.fingerprint,
		policy: judgement.policy.policy,
		allow: [...judgement.policy.allow],
		observed: input.observed ?? null,
		declared: input.declared ?? null,
		recorded: [...(input.recorded ?? [])],
		allowed: verdict.allowed,
		reason: verdict.reason,
		...(verdict.matched === undefined ? {} : { matched: verdict.matched }),
	};
}

/** Everything the judgement reads from the host, and the two calls it may then commit. */
export interface ApprovalDeps {
	policy: AcquisitionPolicy;
	log: AcquisitionLog;
	/** RFC 0018 §2.1: the address the peer's last statement arrived from, as this host recorded it. */
	observed(nodeId: NodeId, fingerprint: string): Promise<string | undefined>;
	/** The address the peer wrote into its own statement, never the address of this call. */
	declared(nodeId: NodeId): Promise<string | undefined>;
	/** Every discovery address recorded for this node id; each is an outbound candidate (§2.2). */
	recorded(nodeId: NodeId): Promise<string[]>;
	/** The audit line, written whether or not the verdict allows the approval (§4.1). */
	audit(judgement: ApprovalJudgement): void;
	accept(nodeId: NodeId, fingerprint: string): Promise<TrustEntry>;
	revoke(nodeId: NodeId): Promise<void>;
	now(): string;
}

export interface ApprovalRequest {
	node_id: NodeId;
	fingerprint: string;
	/** The statement's own connection address, when the caller is holding it. */
	observed?: string;
}

/**
 * RFC 0018 §2: the one judgement, read by both the agent path and the auto-approval path.
 * `observed` is the address a recorded statement arrived from — the caller's live connection
 * address only when it is judging the statement it just received — and the declared address
 * and every recorded address are checked with it (§2.1–2.2).
 */
export async function judgeApproval(deps: ApprovalDeps, request: ApprovalRequest): Promise<ApprovalJudgement> {
	const observed = request.observed ?? await deps.observed(request.node_id, request.fingerprint);
	const declared = await deps.declared(request.node_id);
	const input: AcquireInput = {
		...(observed === undefined || observed === "" ? {} : { observed }),
		...(declared === undefined || declared === "" ? {} : { declared }),
		recorded: await deps.recorded(request.node_id),
	};
	const judgement: ApprovalJudgement = {
		at: deps.now(), node_id: request.node_id, fingerprint: request.fingerprint,
		policy: deps.policy, input, verdict: evaluateAcquisition(deps.policy, input),
	};
	deps.audit(judgement);
	return judgement;
}

/** The refusal an agent-initiated approval gets. A refused approval is not a revocation: the peer's row is left as it is. */
export function approvalRefused(judgement: ApprovalJudgement): OnlyneError {
	const { policy, verdict } = judgement;
	return new OnlyneError("forbidden", hostText("acquireDenied", { policy: policy.policy, reason: verdict.reason }), {
		policy_denied: verdict.reason,
		policy: policy.policy,
		...(judgement.input.observed === undefined ? {} : { observed: judgement.input.observed }),
		...(judgement.input.declared === undefined ? {} : { declared: judgement.input.declared }),
	});
}

/** RFC 0018 §2/§3.1: the agent's `trust_accept`, judged before anything is committed. */
export async function approvePeer(deps: ApprovalDeps, nodeId: NodeId, fingerprint: string): Promise<TrustEntry> {
	const judgement = await judgeApproval(deps, { node_id: nodeId, fingerprint });
	if (!judgement.verdict.allowed) throw approvalRefused(judgement);
	const entry = await deps.accept(nodeId, fingerprint);
	await deps.log.settle(nodeId, fingerprint);
	return entry;
}

/**
 * RFC 0018 §2: the agent's `trust_revoke`, never judged. The policy decides who may come in;
 * it has no say over who may be turned away, which is also why this path reads no address.
 */
export async function revokePeer(deps: ApprovalDeps, nodeId: NodeId): Promise<void> {
	await deps.revoke(nodeId);
	await deps.log.settle(nodeId);
}
