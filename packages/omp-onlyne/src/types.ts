import type { NodeId, ThreadRef, TrustSeed } from "@onlyne/lib";

/**
 * Project-scoped configuration in `<project>/.onlyne/config.toml`.
 *
 * Delivery decisions are made by OMP's `judge` model role, so no model
 * endpoint or model name belongs here.
 */
export interface OnlyneConfig {
	path: string;
	/** Protocol membership, independent from the node identity. */
	region: string;
	node: {
		id: NodeId;
		key_ref?: string;
		key_scope?: "project" | "global";
	};
	server: {
		bind: string;
		/** The externally reachable base URL when a reverse proxy fronts the node. */
		public_url?: string;
	};
	threads: {
		page_size: number;
	};
	/** Bootstrap contact seeds the node reaches at every start. */
	discovery?: {
		seeds?: string[];
	};
	/** Pinned peer fingerprints; an arriving key that matches is trusted without a prompt. */
	trust?: TrustSeed[];
}

/** One execution context the plugin registered for a delivered message. */
export interface ContextRecord {
	alias: string;
	node_id: NodeId;
	main_agent_id: string;
	source?: { sender: NodeId; id: string };
	agent_id?: string;
	session_file?: string;
	parent_id?: string;
	thread_ref?: ThreadRef;
	summary: string;
	state: "unbound" | "idle" | "running" | "parked" | "lost";
	created_at: string;
	bound_at?: string;
}

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
	seq: string;
	direction: "in" | "out";
	author: string;
	at: string;
	text: string;
	relation?: string;
}

/** One thread row in the viewer's chat page. */
export interface ViewerThread {
	alias: string;
	title: string;
	state: "open" | "closed";
	unread: number;
	last_seq: string;
	messages: ViewerMessage[];
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
	trust?: Array<{
		node_id: NodeId;
		fingerprint: string;
		state: "pending" | "trusted" | "revoked";
		base_url: string;
		/** This side approved; the other half of a pending exchange. */
		local_approved: boolean;
		/** The peer approved, which is what makes an invitation inbound. */
		remote_approved: boolean;
	}>;
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
