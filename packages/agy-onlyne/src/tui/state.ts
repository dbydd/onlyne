import type { NodeId, ThreadRef, TrustPhase } from "@onlyne/lib";
import type { AcquirePolicy } from "@onlyne/node-host/acquire";
import type { AgyOnlyneConfig } from "../config.ts";
import type { Locale } from "../i18n.ts";
import { threadKey } from "../core/resolve.ts";
import type {
	OnlyneTodoEntry,
	Snapshot,
	TrustSnapshotEntry,
	ViewerDiscovery,
	ViewerMessage,
	ViewerThread,
} from "../core/types.ts";
import type { BootstrapState } from "./bootstrap.ts";
import { createBootstrapState } from "./bootstrap.ts";
import { shortId } from "./format.ts";

export type TuiPane = "rail" | "sidebar" | "flow" | "members";

export type SidebarItem =
	| { kind: "invite"; entry: TrustSnapshotEntry }
	| { kind: "thread"; thread: ViewerThread }
	| { kind: "new_thread" }
	| { kind: "inbox" }
	| { kind: "trust" }
	| { kind: "todos" };

export type RailItem =
	| { kind: "all" }
	| { kind: "domain"; domainId: string; active: boolean; memberCount: number }
	| { kind: "join_domain" };

export interface PeerEntry {
	node_id: NodeId;
	label: string;
	base_url?: string;
	region?: string;
	last_seen?: string;
	trust?: TrustSnapshotEntry;
	discovery?: ViewerDiscovery;
}

export type TuiModal =
	| { kind: "help" }
	| { kind: "connect"; input: string; busy?: boolean }
	| {
			kind: "compose";
			text: string;
			to?: NodeId;
			thread?: ThreadRef;
			replyTo?: { sender: NodeId; id: string; author: string };
			response: "async" | "await" | "none";
			busy?: boolean;
	  }
	| { kind: "new_thread"; title: string; busy?: boolean }
	| { kind: "join_domain"; domainId: string; busy?: boolean }
	| {
			kind: "exchange";
			nodeId: NodeId;
			baseUrl: string;
			fingerprint: string;
			localFingerprint: string;
			busy?: boolean;
	  }
	| {
			kind: "trust_confirm_action";
			action: "accept" | "revoke";
			nodeId: NodeId;
			fingerprint: string;
			localFingerprint?: string;
			busy?: boolean;
	  }
	| {
			kind: "acquisition";
			policy: AcquirePolicy;
			allowText: string;
			field: 0 | 1;
			busy?: boolean;
	  }
	| {
			kind: "message_detail";
			msg: ViewerMessage;
			thread: ViewerThread;
	  };

export interface TuiNotice {
	tone: "info" | "ok" | "err";
	text: string;
}

export interface TuiState {
	locale: Locale;
	snapshot?: Snapshot;
	activePane: TuiPane;
	selectedDomain?: string;
	railIndex: number;
	sidebarIndex: number;
	selectedThreadKey?: string;
	specialView?: "inbox" | "trust" | "todos";
	flowIndex: number;
	memberIndex: number; // 0 = connect btn, 1 = refresh btn, 2+ = peers
	selectedMemberId?: NodeId;
	modal?: TuiModal;
	bootstrap?: BootstrapState;
	notice?: TuiNotice;
	quitting: boolean;
}

export function createInitialTuiState(locale: Locale = "en"): TuiState {
	return {
		locale,
		activePane: "sidebar",
		railIndex: 0,
		sidebarIndex: 0,
		flowIndex: 0,
		memberIndex: 2,
		quitting: false,
	};
}

export function applySnapshot(state: TuiState, snapshot: Snapshot): TuiState {
	const next: TuiState = {
		...state,
		snapshot,
		locale: snapshot.language,
	};

	const threads = snapshot.threads;
	if (!next.specialView && !next.selectedThreadKey && threads.length > 0) {
		next.selectedThreadKey = threadKey(threads[0]!.ref);
	}
	if (next.selectedThreadKey && !threads.some(t => threadKey(t.ref) === next.selectedThreadKey)) {
		next.selectedThreadKey = threads[0] ? threadKey(threads[0].ref) : undefined;
	}

	const peers = buildPeerList(next);
	if (!next.selectedMemberId && peers.length > 0) {
		next.selectedMemberId = peers[0]!.node_id;
	}
	if (next.selectedMemberId && !peers.some(p => p.node_id === next.selectedMemberId)) {
		next.selectedMemberId = peers[0]?.node_id;
	}
	const peerOffset = peers.findIndex(p => p.node_id === next.selectedMemberId);
	if (peerOffset >= 0 && next.memberIndex >= 2) {
		next.memberIndex = 2 + peerOffset;
	}

	return clampCursors(next);
}

export function buildRailItems(state: TuiState): RailItem[] {
	const items: RailItem[] = [{ kind: "all" }];
	for (const d of state.snapshot?.domains ?? []) {
		items.push({
			kind: "domain",
			domainId: d.domain_id,
			active: d.state === "active",
			memberCount: d.members.length,
		});
	}
	items.push({ kind: "join_domain" });
	return items;
}

export function buildSidebarItems(state: TuiState): SidebarItem[] {
	const snap = state.snapshot;
	if (!snap) return [{ kind: "inbox" }, { kind: "trust" }, { kind: "todos" }];

	const items: SidebarItem[] = [];
	const invited = snap.trust.filter(t => t.phase === "invited");
	for (const entry of invited) {
		items.push({ kind: "invite", entry });
	}

	const selectedDomainObj = state.selectedDomain
		? snap.domains.find(d => d.domain_id === state.selectedDomain)
		: undefined;
	const domainMemberSet = selectedDomainObj ? new Set(selectedDomainObj.members) : undefined;

	const visibleThreads = domainMemberSet
		? snap.threads.filter(t => domainMemberSet.has(t.ref.owner) || t.ref.owner === snap.status.node_id)
		: snap.threads;

	for (const thread of visibleThreads) {
		items.push({ kind: "thread", thread });
	}
	items.push({ kind: "new_thread" });
	items.push({ kind: "inbox" });
	items.push({ kind: "trust" });
	items.push({ kind: "todos" });
	return items;
}

export function buildPeerList(state: TuiState): PeerEntry[] {
	const snap = state.snapshot;
	if (!snap) return [];

	const selectedDomainObj = state.selectedDomain
		? snap.domains.find(d => d.domain_id === state.selectedDomain)
		: undefined;
	const domainMemberSet = selectedDomainObj ? new Set(selectedDomainObj.members) : undefined;

	const byId = new Map<NodeId, PeerEntry>();
	for (const disc of snap.discovery) {
		if (disc.node_id === snap.status.node_id) continue;
		if (domainMemberSet && !domainMemberSet.has(disc.node_id)) continue;
		byId.set(disc.node_id, {
			node_id: disc.node_id,
			label: shortId(disc.node_id),
			base_url: disc.base_url,
			region: disc.region,
			last_seen: disc.last_seen,
			discovery: disc,
		});
	}
	for (const trust of snap.trust) {
		if (trust.node_id === snap.status.node_id) continue;
		if (domainMemberSet && !domainMemberSet.has(trust.node_id)) continue;
		const existing = byId.get(trust.node_id);
		if (existing) {
			existing.trust = trust;
			existing.base_url = existing.base_url ?? trust.url;
			existing.last_seen = existing.last_seen ?? trust.last_seen ?? trust.updated_at;
		} else {
			byId.set(trust.node_id, {
				node_id: trust.node_id,
				label: shortId(trust.node_id),
				base_url: trust.url,
				last_seen: trust.last_seen ?? trust.updated_at,
				trust,
			});
		}
	}

	const phaseOrder: Record<TrustPhase, number> = {
		invited: 0,
		requested: 1,
		exchanged: 2,
		revoked: 4,
	};
	return [...byId.values()].sort((a, b) => {
		const pa = a.trust ? phaseOrder[a.trust.phase] : 3;
		const pb = b.trust ? phaseOrder[b.trust.phase] : 3;
		if (pa !== pb) return pa - pb;
		return a.label.localeCompare(b.label);
	});
}

export function activeThread(state: TuiState): ViewerThread | undefined {
	if (state.specialView) return undefined;
	const threads = state.snapshot?.threads ?? [];
	if (!state.selectedThreadKey) return threads[0];
	return threads.find(t => threadKey(t.ref) === state.selectedThreadKey) ?? threads[0];
}

export function selectedPeer(state: TuiState): PeerEntry | undefined {
	const peers = buildPeerList(state);
	if (peers.length === 0) return undefined;
	if (state.memberIndex >= 2) {
		const atIdx = peers[state.memberIndex - 2];
		if (atIdx) return atIdx;
	}
	if (state.selectedMemberId) {
		return peers.find(p => p.node_id === state.selectedMemberId);
	}
	return peers[0];
}

export function flowItemCount(state: TuiState): number {
	const snap = state.snapshot;
	if (!snap) return 0;
	if (state.specialView === "inbox") {
		return Math.max(1, snap.queue.length + snap.pending_deliveries.length);
	}
	if (state.specialView === "trust") {
		return Math.max(1, snap.audit.length);
	}
	if (state.specialView === "todos") {
		return Math.max(1, snap.todos.length);
	}
	const thread = activeThread(state);
	const invitedCount = snap.trust.filter(t => t.phase === "invited").length;
	const msgCount = thread?.messages.length ?? 0;
	return invitedCount + msgCount;
}

export function clampCursors(state: TuiState): TuiState {
	const railItems = buildRailItems(state);
	const sideItems = buildSidebarItems(state);
	const peers = buildPeerList(state);
	const fCount = flowItemCount(state);

	const railIndex = Math.max(0, Math.min(state.railIndex, Math.max(0, railItems.length - 1)));
	const sidebarIndex = Math.max(0, Math.min(state.sidebarIndex, Math.max(0, sideItems.length - 1)));
	const maxMemberIdx = Math.max(1, 2 + peers.length - 1);
	const memberIndex = Math.max(0, Math.min(state.memberIndex, maxMemberIdx));
	const flowIndex = Math.max(0, Math.min(state.flowIndex, Math.max(0, fCount - 1)));

	const peerFromIdx = memberIndex >= 2 ? peers[memberIndex - 2]?.node_id : state.selectedMemberId;

	return {
		...state,
		railIndex,
		sidebarIndex,
		memberIndex,
		flowIndex,
		selectedMemberId: peerFromIdx ?? peers[0]?.node_id,
	};
}

export function nextPane(pane: TuiPane, dir: 1 | -1): TuiPane {
	const order: TuiPane[] = ["rail", "sidebar", "flow", "members"];
	const idx = order.indexOf(pane);
	return order[(idx + dir + order.length) % order.length]!;
}

export function moveCursor(state: TuiState, delta: number): TuiState {
	if (state.activePane === "rail") {
		return clampCursors({ ...state, railIndex: state.railIndex + delta });
	}
	if (state.activePane === "sidebar") {
		const next = clampCursors({ ...state, sidebarIndex: state.sidebarIndex + delta });
		const item = buildSidebarItems(next)[next.sidebarIndex];
		if (item?.kind === "thread") {
			return {
				...next,
				specialView: undefined,
				selectedThreadKey: threadKey(item.thread.ref),
				flowIndex: Math.max(0, item.thread.messages.length - 1),
			};
		}
		if (item?.kind === "inbox" || item?.kind === "trust" || item?.kind === "todos") {
			return { ...next, specialView: item.kind, flowIndex: 0 };
		}
		return next;
	}
	if (state.activePane === "flow") {
		return clampCursors({ ...state, flowIndex: state.flowIndex + delta });
	}
	if (state.activePane === "members") {
		const next = clampCursors({ ...state, memberIndex: state.memberIndex + delta });
		return next;
	}
	return state;
}

export function openComposeForSelection(
	state: TuiState,
	mode: "new" | "reply",
	replyMsg?: ViewerMessage,
): { state: TuiState; blocked?: "readonly" | "empty" } {
	const snap = state.snapshot;
	if (!snap) return { state, blocked: "empty" };
	const thread = activeThread(state);

	if (mode === "reply" && thread) {
		if (thread.read_only) {
			return { state, blocked: "readonly" };
		}
		const targetMsg = replyMsg ?? thread.messages[thread.messages.length - 1];
		if (targetMsg) {
			return {
				state: {
					...state,
					modal: {
						kind: "compose",
						text: "",
						thread: thread.ref,
						replyTo: {
							sender: targetMsg.ref.sender,
							id: targetMsg.ref.id,
							author: shortId(targetMsg.author),
						},
						response: "async",
					},
				},
			};
		}
	}

	if (!state.specialView && thread && state.activePane !== "members") {
		if (thread.read_only) {
			const peer = selectedPeer(state);
			if (!peer) return { state, blocked: "readonly" };
			return {
				state: {
					...state,
					modal: {
						kind: "compose",
						text: "",
						to: peer.node_id,
						response: "async",
					},
				},
			};
		}
		return {
			state: {
				...state,
				modal: {
					kind: "compose",
					text: "",
					thread: thread.ref,
					response: "async",
				},
			},
		};
	}

	const peer = selectedPeer(state);
	if (peer) {
		return {
			state: {
				...state,
				modal: {
					kind: "compose",
					text: "",
					to: peer.node_id,
					response: "async",
				},
			},
		};
	}

	if (thread && !thread.read_only) {
		return {
			state: {
				...state,
				modal: {
					kind: "compose",
					text: "",
					thread: thread.ref,
					response: "async",
				},
			},
		};
	}
	return { state, blocked: thread?.read_only ? "readonly" : "empty" };
}

export function openBootstrapModal(state: TuiState, workspace: string, allowCancel = true): TuiState {
	const snap = state.snapshot;
	const current: AgyOnlyneConfig | undefined = snap?.configured
		? {
				path: `${workspace}/.onlyne/config.toml`,
				region: snap.status.region,
				language: snap.language,
				node: {
					id: snap.status.node_id,
					key_scope: "project",
				},
				server: {
					bind: "127.0.0.1",
				},
				threads: {
					page_size: 50,
				},
				acquisition: {
					policy: snap.acquisition.policy,
					allow: [...snap.acquisition.allow],
				},
				discovery: {
					seeds: snap.discovery.filter(d => d.source === "seed").map(d => d.base_url),
				},
				judge: {
					mode: snap.judge_mode as NonNullable<AgyOnlyneConfig["judge"]>["mode"],
				},
		  }
		: undefined;
	return {
		...state,
		bootstrap: createBootstrapState({ workspace, current, allowCancel }),
	};
}

export function nextTodoStatus(current: OnlyneTodoEntry["status"]): OnlyneTodoEntry["status"] {
	if (current === "pending") return "in_progress";
	if (current === "in_progress") return "completed";
	if (current === "completed") return "blocked";
	return "pending";
}
