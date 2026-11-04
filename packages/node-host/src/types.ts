import type { NodeId, ThreadRef, TrustSeed } from "@onlyne/lib";
import type { Locale } from "./i18n.ts";
import type { AcquisitionPolicy } from "./acquire.ts";

/**
 * Project-scoped configuration in `<project>/.onlyne/config.toml`.
 *
 * Delivery decisions are made by OMP's `judge` model role, so no model
 * endpoint or model name belongs here.
 */
export interface OnlyneConfig {
	path: string;
	language?: Locale;
	/** Protocol membership, independent from the node identity. */
	region: string;
	node: {
		id: NodeId;
		key_ref?: string;
		/** Where the node identity lives: the project's `.onlyne/keys`, the user's key store, or no key at all. */
		key_scope?: "project" | "global" | "none";
	};
	server: {
		bind: string;
		/**
		 * The port this node answers on. Absent asks the operating system for one, and the
		 * chosen port is written back here: a node whose address changes on every start
		 * leaves every peer that recorded it calling a port nobody serves.
		 */
		port?: number;
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
	/** RFC 0018 §2: peers this node may approve without a human confirming first. */
	acquisition?: AcquisitionPolicy;
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
