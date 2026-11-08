import type { MessageRef, NodeId, ThreadRef, TrustPhase, WorkRef } from "@onlyne/lib";
import type { AcquirePolicy } from "@onlyne/node-host/acquire";
import type { ContextRecord } from "@onlyne/node-host/types";

/** One row of the viewer's queue page. */
export interface QueueItem {
	id: string;
	kind: "inbound" | "outbound" | "context" | "decision";
	label: string;
	detail: string;
	at?: string;
}

/** One message row in the viewer's chat page. */
export interface ViewerMessage {
	ref: MessageRef;
	seq: string;
	direction: "in" | "out";
	author: string;
	at: string;
	text: string;
	relation?: string;
	/** The work unit the sender named (RFC 0003 §1), when the message carries one. */
	work?: WorkRef;
}

/** One thread row in the viewer's chat page. */
export interface ViewerThread {
	ref: ThreadRef;
	alias: string;
	title: string;
	state: "open" | "closed";
	unread: number;
	last_seq: string;
	messages: ViewerMessage[];
	/**
	 * A thread another node hosts: this node was invited into it, so the viewer can show it and
	 * cannot offer the writes that belong to its owner.
	 */
	read_only?: boolean;
}

/** One discovery record row the viewer lists as an automatic-discovery candidate. */
export interface ViewerDiscovery {
	node_id: NodeId;
	instance_id: string;
	base_url: string;
	source: string;
	region?: string;
	membership_state?: "active" | "left";
	verified_at?: string;
	expires_at: string;
	last_seen: string;
	error?: string;
}

/** One reachable peer the viewer lists on its contacts page. */
export interface ViewerContact {
	node_id: NodeId;
	instance_id: string;
	base_url: string;
	region?: string;
	last_seen: string;
}

/** RFC 0018 §5: the acquisition settings this runtime actually runs with, read back for the panel. */
export interface AcquisitionSnapshot {
	policy: AcquirePolicy;
	allow: string[];
	/**
	 * The discovery layer's separate loopback switch (RFC 0018 §5.1). It decides what may enter
	 * the catalog, not what may be approved without a human, and the panel shows both values
	 * together so loosening one is never mistaken for loosening the other.
	 */
	allow_loopback: boolean;
}

/**
 * One acquisition audit row. The actors stay distinguishable (RFC 0018 §5): a human's panel
 * action, an agent's decision, a configured seed's pre-authorization, and the policy's own
 * automatic verdict. `allowed` is the verdict the gate reached; `override` marks a human
 * approval that committed although the policy refused it, and a seed authorization that
 * bypasses the policy by design.
 */
export interface AcquisitionAuditEntry {
	at: string;
	actor: "human" | "agent" | "seed" | "policy";
	action: string;
	node_id?: NodeId;
	fingerprint?: string;
	/** The address the peer's statement arrived from, as this host observed it. */
	observed?: string;
	/** The address the peer wrote into its own statement. */
	declared?: string;
	policy?: AcquirePolicy;
	allowed?: boolean;
	reason?: string;
	override?: boolean;
	/** The configuration file a seed authorization came from. */
	source?: string;
}

/**
 * One trust row with both addresses the panel needs. `base_url` is what the peer declared;
 * `observed` is where its last statement actually arrived from, and the two need not agree
 * (RFC 0018 §2.1).
 */
export interface ViewerTrustRow {
	node_id: NodeId;
	fingerprint: string;
	phase: TrustPhase;
	base_url: string;
	observed?: string;
}

/** Read-only snapshot the viewer renders. */
export interface ViewerSnapshot {
	region?: string;
	endpoint?: string;
	queue: QueueItem[];
	threads: ViewerThread[];
	contexts: ContextRecord[];
	discovery: ViewerDiscovery[];
	contacts: ViewerContact[];
	discovery_error?: string;
	/** Every trust row, projected through `trustPhase`: the only stage this package renders. */
	trust?: ViewerTrustRow[];
	/** RFC 0018 §5: the acquisition panel's current settings. */
	acquisition?: AcquisitionSnapshot;
	/** The persisted acquisition audit, newest first; the panel renders the first 20. */
	audit?: AcquisitionAuditEntry[];
	identity?: { public_key: string; fingerprint: string };
}

/** The one-line runtime status a status widget renders. */
export interface OnlyneStatus {
	region: string;
	node_id: NodeId;
	nodes: number;
	neighbors: number;
	alerts: number;
	unread: number;
	discovery_error?: string;
}
