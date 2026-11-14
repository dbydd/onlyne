import type { MessageRef, NodeId, ThreadRef, TrustPhase, WorkRef } from "@onlyne/lib";
import type { AcquirePolicy } from "@onlyne/node-host/acquire";
import type { ContextRecord } from "@onlyne/node-host/types";

export interface QueueItem {
	id: string;
	kind: "inbound" | "outbound" | "context" | "decision";
	label: string;
	detail: string;
	at?: string;
}

export interface ViewerMessage {
	ref: MessageRef;
	seq: string;
	direction: "in" | "out";
	author: string;
	at: string;
	text: string;
	relation?: string;
	work?: WorkRef;
}

export interface ViewerThread {
	ref: ThreadRef;
	alias: string;
	title: string;
	state: "open" | "closed";
	unread: number;
	last_seq: string;
	messages: ViewerMessage[];
	read_only?: boolean;
}

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

export interface ViewerContact {
	node_id: NodeId;
	instance_id: string;
	base_url: string;
	region?: string;
	last_seen: string;
}

export interface AcquisitionSnapshot {
	policy: AcquirePolicy;
	allow: string[];
	allow_loopback: boolean;
}

export interface AcquisitionAuditEntry {
	at: string;
	actor: "human" | "agent" | "seed" | "policy";
	action: string;
	node_id?: NodeId;
	fingerprint?: string;
	observed?: string;
	declared?: string;
	policy?: AcquirePolicy;
	allowed?: boolean;
	reason?: string;
	override?: boolean;
	source?: string;
}

export interface OnlyneStatus {
	node_id: NodeId;
	region: string;
	endpoint: string;
	domains: string[];
	peers_total: number;
	peers_reachable: number;
	trusted: number;
	pending_trust: number;
	pending_keys: number;
	pending_approvals: number;
	unread: number;
	inbox_count: number;
	outbound_pending: number;
	contexts_active: number;
	decisions_pending: number;
}

export interface TrustSnapshotEntry {
	node_id: NodeId;
	phase: TrustPhase;
	fingerprint: string;
	local_fingerprint?: string;
	url?: string;
	verified: boolean;
	updated_at: string;
	last_seen?: string;
	peer_confirmed?: boolean;
	local_confirmed?: boolean;
}

export interface DomainSnapshotEntry {
	domain_id: string;
	state: "active" | "left";
	revision: string;
	members: NodeId[];
	updated_at: string;
}

export interface OnlyneTodoEntry {
	id: string;
	phase: string;
	title: string;
	status: "pending" | "in_progress" | "completed" | "blocked";
	source_sender?: NodeId;
	source_id?: string;
	thread_key?: string;
	explanation?: string;
	updated_at: string;
}

export interface DeliveryPendingEntry {
	id: string;
	token: string;
	conversation_id?: string;
	context_alias: string;
	sender: NodeId;
	message_id: string;
	thread_ref: ThreadRef;
	delivery_kind: string;
	text: string;
	plan_response: "await" | "async" | "none";
	plan_execution: "existing" | "new";
	attempt: number;
	created_at: string;
	woke_at?: string;
}

export interface Snapshot {
	at: string;
	workspace: string;
	configured: boolean;
	language: "en" | "zh-CN";
	status: OnlyneStatus;
	fingerprint?: string;
	acquisition: AcquisitionSnapshot;
	audit: AcquisitionAuditEntry[];
	threads: ViewerThread[];
	trust: TrustSnapshotEntry[];
	domains: DomainSnapshotEntry[];
	discovery: ViewerDiscovery[];
	contacts: ViewerContact[];
	contexts: ContextRecord[];
	queue: QueueItem[];
	todos: OnlyneTodoEntry[];
	pending_deliveries: DeliveryPendingEntry[];
	judge_mode: string;
}
