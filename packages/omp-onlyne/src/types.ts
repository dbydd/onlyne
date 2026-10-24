import type { NodeId, ThreadRef } from "@onlyne/lib";

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
	};
	server: {
		bind: string;
	};
	threads: {
		page_size: number;
	};
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

/** Read-only snapshot the viewer renders. */
export interface ViewerSnapshot {
	region?: string;
	endpoint?: string;
	queue: QueueItem[];
	threads: ViewerThread[];
	contexts: ContextRecord[];
}
