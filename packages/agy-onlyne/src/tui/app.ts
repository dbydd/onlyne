import {
	BOOTSTRAP_FIELDS,
	bootstrapFocusLimit,
	validateBootstrapValues,
} from "./bootstrap.ts";
import { renderTuiFrame } from "./render.ts";
import { threadKey } from "../core/resolve.ts";
import {
	activeThread,
	applySnapshot,
	buildPeerList,
	buildRailItems,
	buildSidebarItems,
	clampCursors,
	createInitialTuiState,
	moveCursor,
	nextPane,
	nextTodoStatus,
	openBootstrapModal,
	openComposeForSelection,
	selectedPeer,
	type TuiState,
} from "./state.ts";
import { tuiText } from "./text.ts";
import type { DaemonClient } from "../runtime/daemon-client.ts";

export interface DecodedKey {
	name: string;
	char?: string;
	ctrl?: boolean;
	shift?: boolean;
}

export function decodeKeyEvents(raw: string): DecodedKey[] {
	const out: DecodedKey[] = [];
	let i = 0;
	while (i < raw.length) {
		const ch = raw[i]!;
		if (ch === "\x1b") {
			if (raw.startsWith("\x1b[A", i)) {
				out.push({ name: "up" });
				i += 3;
				continue;
			}
			if (raw.startsWith("\x1b[B", i)) {
				out.push({ name: "down" });
				i += 3;
				continue;
			}
			if (raw.startsWith("\x1b[C", i)) {
				out.push({ name: "right" });
				i += 3;
				continue;
			}
			if (raw.startsWith("\x1b[D", i)) {
				out.push({ name: "left" });
				i += 3;
				continue;
			}
			if (raw.startsWith("\x1b[Z", i)) {
				out.push({ name: "tab", shift: true });
				i += 3;
				continue;
			}
			out.push({ name: "escape" });
			i += 1;
			continue;
		}
		if (ch === "\r" || ch === "\n") {
			out.push({ name: "enter" });
			i += 1;
			continue;
		}
		if (ch === "\t") {
			out.push({ name: "tab" });
			i += 1;
			continue;
		}
		if (ch === "\x7f" || ch === "\b") {
			out.push({ name: "backspace" });
			i += 1;
			continue;
		}
		if (ch === "\x03") {
			out.push({ name: "c", ctrl: true });
			i += 1;
			continue;
		}
		if (ch === "\x13") {
			out.push({ name: "s", ctrl: true });
			i += 1;
			continue;
		}
		const code = ch.codePointAt(0) ?? 0;
		if (code >= 1 && code <= 26) {
			out.push({ name: String.fromCharCode(96 + code), ctrl: true });
			i += 1;
			continue;
		}
		if (code >= 32) {
			const cpLen = ch.length;
			out.push({ name: ch, char: ch });
			i += cpLen;
			continue;
		}
		i += 1;
	}
	return out;
}

export interface TuiDaemonActions {
	refresh(): Promise<unknown>;
	connect(url: string): Promise<unknown>;
	send(payload: {
		delivery_kind?: string;
		msg: string;
		to?: string;
		thread?: { owner: string; id: string };
		in_reply_to?: { sender: string; id: string };
		relation?: "reply" | "followup";
		response?: "await" | "async" | "none";
	}): Promise<unknown>;
	trustBegin(nodeId: string): Promise<{ node_id: string; fingerprint: string; base_url: string }>;
	trustConfirm(nodeId: string, fingerprint: string): Promise<unknown>;
	trustAccept(nodeId: string): Promise<unknown>;
	trustRevoke(nodeId: string): Promise<unknown>;
	trustRotate(): Promise<unknown>;
	setAcquisition(policy: "all" | "segment" | "none", allow: string[]): Promise<unknown>;
	setLanguage(lang: "en" | "zh-CN"): Promise<unknown>;
	domainAction(op: "join" | "leave", domainId: string): Promise<unknown>;
	createThread(title: string): Promise<unknown>;
	updateTodo(id: string, status: "pending" | "in_progress" | "completed" | "blocked"): Promise<unknown>;
	saveConfig(payload: Record<string, unknown>): Promise<{ ok: boolean; snapshot: import("../core/types.ts").Snapshot }>;
	snapshot(): Promise<import("../core/types.ts").Snapshot>;
}

function handleBootstrapKey(
	state: TuiState,
	key: DecodedKey,
	workspace: string,
	actions: TuiDaemonActions,
	onAsyncUpdate: (fn: (cur: TuiState) => TuiState) => void,
): TuiState {
	const bs = state.bootstrap;
	if (!bs) return state;
	const strings = tuiText(state.locale);
	const limit = bootstrapFocusLimit(bs);

	if (key.ctrl && key.name === "c") {
		if (bs.allowCancel) return { ...state, bootstrap: undefined };
		return { ...state, quitting: true };
	}
	if (key.name === "escape" && bs.allowCancel) {
		return { ...state, bootstrap: undefined };
	}

	if (key.name === "down" || (key.name === "tab" && !key.shift)) {
		return { ...state, bootstrap: { ...bs, focusIndex: (bs.focusIndex + 1) % limit, error: undefined } };
	}
	if (key.name === "up" || (key.name === "tab" && key.shift)) {
		return { ...state, bootstrap: { ...bs, focusIndex: (bs.focusIndex - 1 + limit) % limit, error: undefined } };
	}

	const triggerSave = (): TuiState => {
		if (bs.saving) return state;
		const check = validateBootstrapValues(bs.values, state.locale);
		if (!check.ok) {
			return { ...state, bootstrap: { ...bs, error: check.error } };
		}
		void actions.saveConfig({ ...check.payload, language: state.locale }).then(
			res => {
				onAsyncUpdate(cur => applySnapshot({
					...cur,
					bootstrap: undefined,
					notice: { tone: "ok", text: strings.bootstrap.saved },
				}, res.snapshot));
			},
			err => {
				onAsyncUpdate(cur => cur.bootstrap
					? { ...cur, bootstrap: { ...cur.bootstrap, saving: false, error: err instanceof Error ? err.message : String(err) } }
					: cur);
			},
		);
		return { ...state, bootstrap: { ...bs, saving: true, error: undefined } };
	};

	if (key.ctrl && key.name === "s") {
		return triggerSave();
	}

	if (bs.focusIndex === BOOTSTRAP_FIELDS.length) {
		if (key.name === "enter" || key.char === " ") return triggerSave();
		return state;
	}
	if (bs.focusIndex === BOOTSTRAP_FIELDS.length + 1 && bs.allowCancel) {
		if (key.name === "enter" || key.char === " ") {
			return { ...state, bootstrap: undefined };
		}
		return state;
	}

	const field = BOOTSTRAP_FIELDS[bs.focusIndex];
	if (!field) return state;

	if (key.name === "enter") {
		return { ...state, bootstrap: { ...bs, focusIndex: (bs.focusIndex + 1) % limit, error: undefined } };
	}
	if (key.name === "backspace") {
		const prev = bs.values[field];
		return {
			...state,
			bootstrap: {
				...bs,
				values: { ...bs.values, [field]: prev.slice(0, -1) },
				error: undefined,
			},
		};
	}
	if (key.char) {
		const prev = bs.values[field];
		return {
			...state,
			bootstrap: {
				...bs,
				values: { ...bs.values, [field]: prev + key.char },
				error: undefined,
			},
		};
	}
	void workspace;
	return state;
}

function handleModalKey(
	state: TuiState,
	key: DecodedKey,
	actions: TuiDaemonActions,
	onAsyncUpdate: (fn: (cur: TuiState) => TuiState) => void,
): TuiState {
	const modal = state.modal;
	if (!modal) return state;
	const strings = tuiText(state.locale);

	if (key.name === "escape" || (key.ctrl && key.name === "c")) {
		return { ...state, modal: undefined };
	}

	if (modal.kind === "help") {
		if (key.name === "enter" || key.char === "?" || key.char === "q") {
			return { ...state, modal: undefined };
		}
		return state;
	}

	if (modal.kind === "message_detail") {
		if (key.char === "y") {
			const res = openComposeForSelection({ ...state, modal: undefined }, "reply", modal.msg);
			return res.state;
		}
		if (key.char === "c") {
			return {
				...state,
				modal: {
					kind: "compose",
					text: "",
					to: modal.msg.author,
					response: "async",
				},
			};
		}
		return state;
	}

	if (modal.kind === "connect") {
		if (key.name === "enter" && modal.input.trim().length > 0 && !modal.busy) {
			const url = modal.input.trim();
			void actions.connect(url).then(
				async () => {
					const snap = await actions.snapshot();
					onAsyncUpdate(cur => applySnapshot({
						...cur,
						modal: undefined,
						notice: { tone: "ok", text: `${strings.notices.connected}${url}` },
					}, snap));
				},
				err => {
					onAsyncUpdate(cur => ({
						...cur,
						modal: cur.modal?.kind === "connect" ? { ...cur.modal, busy: false } : cur.modal,
						notice: { tone: "err", text: err instanceof Error ? err.message : String(err) },
					}));
				},
			);
			return { ...state, modal: { ...modal, busy: true } };
		}
		if (key.name === "backspace") {
			return { ...state, modal: { ...modal, input: modal.input.slice(0, -1) } };
		}
		if (key.char) {
			return { ...state, modal: { ...modal, input: modal.input + key.char } };
		}
		return state;
	}

	if (modal.kind === "new_thread") {
		if (key.name === "enter" && modal.title.trim().length > 0 && !modal.busy) {
			const title = modal.title.trim();
			void actions.createThread(title).then(
				async () => {
					const snap = await actions.snapshot();
					onAsyncUpdate(cur => applySnapshot({
						...cur,
						modal: undefined,
						notice: { tone: "ok", text: strings.notices.threadCreated },
					}, snap));
				},
				err => {
					onAsyncUpdate(cur => ({
						...cur,
						modal: cur.modal?.kind === "new_thread" ? { ...cur.modal, busy: false } : cur.modal,
						notice: { tone: "err", text: err instanceof Error ? err.message : String(err) },
					}));
				},
			);
			return { ...state, modal: { ...modal, busy: true } };
		}
		if (key.name === "backspace") return { ...state, modal: { ...modal, title: modal.title.slice(0, -1) } };
		if (key.char) return { ...state, modal: { ...modal, title: modal.title + key.char } };
		return state;
	}

	if (modal.kind === "join_domain") {
		if (key.name === "enter" && modal.domainId.trim().length > 0 && !modal.busy) {
			const domainId = modal.domainId.trim();
			void actions.domainAction("join", domainId).then(
				async () => {
					const snap = await actions.snapshot();
					onAsyncUpdate(cur => applySnapshot({
						...cur,
						modal: undefined,
						notice: { tone: "ok", text: strings.notices.domainJoined },
					}, snap));
				},
				err => {
					onAsyncUpdate(cur => ({
						...cur,
						modal: cur.modal?.kind === "join_domain" ? { ...cur.modal, busy: false } : cur.modal,
						notice: { tone: "err", text: err instanceof Error ? err.message : String(err) },
					}));
				},
			);
			return { ...state, modal: { ...modal, busy: true } };
		}
		if (key.name === "backspace") return { ...state, modal: { ...modal, domainId: modal.domainId.slice(0, -1) } };
		if (key.char) return { ...state, modal: { ...modal, domainId: modal.domainId + key.char } };
		return state;
	}

	if (modal.kind === "compose") {
		if (key.name === "tab") {
			const order: Array<"async" | "await" | "none"> = ["async", "await", "none"];
			const next = order[(order.indexOf(modal.response) + 1) % order.length]!;
			return { ...state, modal: { ...modal, response: next } };
		}
		if (key.ctrl && key.name === "s" && !modal.busy) {
			const text = modal.text.trim();
			if (!text) return state;
			void actions.send({
				msg: text,
				to: modal.to,
				thread: modal.thread,
				in_reply_to: modal.replyTo ? { sender: modal.replyTo.sender, id: modal.replyTo.id } : undefined,
				relation: modal.replyTo ? "reply" : undefined,
				response: modal.response,
			}).then(
				async () => {
					const snap = await actions.snapshot();
					onAsyncUpdate(cur => applySnapshot({
						...cur,
						modal: undefined,
						notice: { tone: "ok", text: strings.notices.sent },
					}, snap));
				},
				err => {
					onAsyncUpdate(cur => ({
						...cur,
						modal: cur.modal?.kind === "compose" ? { ...cur.modal, busy: false } : cur.modal,
						notice: { tone: "err", text: err instanceof Error ? err.message : String(err) },
					}));
				},
			);
			return { ...state, modal: { ...modal, busy: true } };
		}
		if (key.name === "enter") return { ...state, modal: { ...modal, text: `${modal.text}\n` } };
		if (key.name === "backspace") return { ...state, modal: { ...modal, text: modal.text.slice(0, -1) } };
		if (key.char) return { ...state, modal: { ...modal, text: modal.text + key.char } };
		return state;
	}

	if (modal.kind === "exchange") {
		if (key.name === "enter" && !modal.busy) {
			void actions.trustConfirm(modal.nodeId, modal.fingerprint).then(
				async () => {
					const snap = await actions.snapshot();
					onAsyncUpdate(cur => applySnapshot({
						...cur,
						modal: undefined,
						notice: { tone: "ok", text: strings.notices.exchangeSent },
					}, snap));
				},
				err => {
					onAsyncUpdate(cur => ({
						...cur,
						modal: undefined,
						notice: { tone: "err", text: err instanceof Error ? err.message : String(err) },
					}));
				},
			);
			return { ...state, modal: { ...modal, busy: true } };
		}
		return state;
	}

	if (modal.kind === "trust_confirm_action") {
		if (key.name === "enter" && !modal.busy) {
			const call = modal.action === "accept"
				? actions.trustAccept(modal.nodeId)
				: actions.trustRevoke(modal.nodeId);
			void call.then(
				async () => {
					const snap = await actions.snapshot();
					onAsyncUpdate(cur => applySnapshot({
						...cur,
						modal: undefined,
						notice: {
							tone: "ok",
							text: modal.action === "accept" ? strings.notices.trusted : strings.notices.revoked,
						},
					}, snap));
				},
				err => {
					onAsyncUpdate(cur => ({
						...cur,
						modal: undefined,
						notice: { tone: "err", text: err instanceof Error ? err.message : String(err) },
					}));
				},
			);
			return { ...state, modal: { ...modal, busy: true } };
		}
		return state;
	}

	if (modal.kind === "acquisition") {
		if (key.name === "tab") {
			return { ...state, modal: { ...modal, field: modal.field === 0 ? 1 : 0 } };
		}
		if (modal.field === 0 && (key.char === " " || key.name === "left" || key.name === "right")) {
			const order: Array<"all" | "segment" | "none"> = ["all", "segment", "none"];
			const nextPolicy = order[(order.indexOf(modal.policy) + 1) % order.length]!;
			return { ...state, modal: { ...modal, policy: nextPolicy } };
		}
		if (modal.field === 1 && key.name === "backspace") {
			return { ...state, modal: { ...modal, allowText: modal.allowText.slice(0, -1) } };
		}
		if (modal.field === 1 && key.char) {
			return { ...state, modal: { ...modal, allowText: modal.allowText + key.char } };
		}
		if ((key.name === "enter" || (key.ctrl && key.name === "s")) && !modal.busy) {
			const allow = modal.allowText.split(/[,\s]+/).map(s => s.trim()).filter(Boolean);
			void actions.setAcquisition(modal.policy, allow).then(
				async () => {
					const snap = await actions.snapshot();
					onAsyncUpdate(cur => applySnapshot({
						...cur,
						modal: undefined,
						notice: { tone: "ok", text: strings.notices.acqUpdated },
					}, snap));
				},
				err => {
					onAsyncUpdate(cur => ({
						...cur,
						modal: cur.modal?.kind === "acquisition" ? { ...cur.modal, busy: false } : cur.modal,
						notice: { tone: "err", text: err instanceof Error ? err.message : String(err) },
					}));
				},
			);
			return { ...state, modal: { ...modal, busy: true } };
		}
		return state;
	}

	return state;
}

export function dispatchTuiKey(
	state: TuiState,
	key: DecodedKey,
	workspace: string,
	actions: TuiDaemonActions,
	onAsyncUpdate: (fn: (cur: TuiState) => TuiState) => void,
): TuiState {
	if (state.bootstrap) {
		return handleBootstrapKey(state, key, workspace, actions, onAsyncUpdate);
	}
	if (state.modal) {
		return handleModalKey(state, key, actions, onAsyncUpdate);
	}

	const strings = tuiText(state.locale);

	if ((key.ctrl && key.name === "c") || key.char === "q") {
		return { ...state, quitting: true };
	}
	if (key.char === "?") {
		return { ...state, modal: { kind: "help" } };
	}
	if (key.char === "i") {
		return openBootstrapModal(state, workspace, true);
	}
	if (key.char === "L") {
		const nextLang = state.locale === "zh-CN" ? "en" : "zh-CN";
		void actions.setLanguage(nextLang).then(async () => {
			const snap = await actions.snapshot();
			onAsyncUpdate(cur => applySnapshot(cur, snap));
		});
		return { ...state, locale: nextLang };
	}
	if (key.char === "r" || (key.ctrl && key.name === "r")) {
		void actions.refresh().then(
			async () => {
				const snap = await actions.snapshot();
				onAsyncUpdate(cur => applySnapshot({
					...cur,
					notice: { tone: "ok", text: strings.notices.reloaded },
				}, snap));
			},
			err => {
				onAsyncUpdate(cur => ({ ...cur, notice: { tone: "err", text: err instanceof Error ? err.message : String(err) } }));
			},
		);
		return { ...state, notice: { tone: "info", text: strings.loading } };
	}
	if (key.char === "s") {
		return { ...state, modal: { kind: "connect", input: "http://" } };
	}

	// Column switching
	if (key.char === "1") return { ...state, activePane: "rail" };
	if (key.char === "2") return { ...state, activePane: "sidebar" };
	if (key.char === "3") return { ...state, activePane: "flow" };
	if (key.char === "4") return { ...state, activePane: "members" };
	if (key.name === "tab") {
		return { ...state, activePane: nextPane(state.activePane, key.shift ? -1 : 1) };
	}
	if (key.name === "left" || key.char === "h") {
		return { ...state, activePane: nextPane(state.activePane, -1) };
	}
	if (key.name === "right" || key.char === "l") {
		return { ...state, activePane: nextPane(state.activePane, 1) };
	}

	// Cursor navigation
	if (key.name === "up" || key.char === "k") {
		// In members pane or when pressing 'k', handle carefully:
		// ArrowUp moves cursor up everywhere; 'k' does key exchange when in members pane or on invited peer!
		if (key.name === "up") return moveCursor(state, -1);
		if (state.activePane !== "members" && !(state.activePane === "flow" && (state.snapshot?.trust.some(t => t.phase === "invited") ?? false))) {
			return moveCursor(state, -1);
		}
	}
	if (key.name === "down" || key.char === "j") {
		return moveCursor(state, 1);
	}

	// Compose & reply
	if (key.char === "c") {
		const res = openComposeForSelection(state, "new");
		if (res.blocked === "readonly") {
			return { ...state, notice: { tone: "err", text: strings.notices.readOnlyThread } };
		}
		if (res.blocked === "empty") {
			return { ...state, notice: { tone: "err", text: strings.notices.noComposeTarget } };
		}
		return res.state;
	}
	if (key.char === "y") {
		const res = openComposeForSelection(state, "reply");
		if (res.blocked === "readonly") {
			return { ...state, notice: { tone: "err", text: strings.notices.readOnlyThread } };
		}
		if (res.blocked === "empty") {
			return { ...state, notice: { tone: "err", text: strings.notices.noComposeTarget } };
		}
		return res.state;
	}

	// Trust actions
	if (key.char === "k") {
		const peer = selectedPeer(state);
		if (!peer) {
			return { ...state, notice: { tone: "err", text: strings.notices.noNodeSelected } };
		}
		void actions.trustBegin(peer.node_id).then(
			res => {
				onAsyncUpdate(cur => ({
					...cur,
					modal: {
						kind: "exchange",
						nodeId: res.node_id,
						baseUrl: res.base_url,
						fingerprint: res.fingerprint,
						localFingerprint: cur.snapshot?.fingerprint ?? "(none)",
					},
				}));
			},
			err => {
				onAsyncUpdate(cur => ({
					...cur,
					notice: { tone: "err", text: err instanceof Error ? err.message : String(err) },
				}));
			},
		);
		return { ...state, notice: { tone: "info", text: strings.loading } };
	}

	if (key.char === "a") {
		const invited = (state.snapshot?.trust ?? []).filter(t => t.phase === "invited");
		const peer = selectedPeer(state);
		const target = (state.activePane === "members" && peer?.trust?.phase === "invited" ? peer.trust : undefined)
			?? invited[0];
		if (!target) return state;
		return {
			...state,
			modal: {
				kind: "trust_confirm_action",
				action: "accept",
				nodeId: target.node_id,
				fingerprint: target.fingerprint,
				localFingerprint: target.local_fingerprint ?? state.snapshot?.fingerprint,
			},
		};
	}

	if (key.char === "x") {
		const peer = selectedPeer(state);
		const invited = (state.snapshot?.trust ?? []).filter(t => t.phase === "invited");
		const target = (state.activePane === "members" ? peer?.trust : undefined) ?? invited[0] ?? peer?.trust;
		if (!target) return state;
		return {
			...state,
			modal: {
				kind: "trust_confirm_action",
				action: "revoke",
				nodeId: target.node_id,
				fingerprint: target.fingerprint,
				localFingerprint: target.local_fingerprint ?? state.snapshot?.fingerprint,
			},
		};
	}

	if (key.char === "K") {
		void actions.trustRotate().then(
			async () => {
				const snap = await actions.snapshot();
				onAsyncUpdate(cur => applySnapshot({
					...cur,
					notice: { tone: "ok", text: strings.notices.keyRotated },
				}, snap));
			},
			err => {
				onAsyncUpdate(cur => ({ ...cur, notice: { tone: "err", text: err instanceof Error ? err.message : String(err) } }));
			},
		);
		return state;
	}

	if (key.char === "e") {
		const acq = state.snapshot?.acquisition;
		return {
			...state,
			modal: {
				kind: "acquisition",
				policy: acq?.policy ?? "segment",
				allowText: (acq?.allow ?? []).join(", "),
				field: 0,
			},
		};
	}

	// Enter / Space activation
	if (key.name === "enter" || key.char === " ") {
		if (state.activePane === "rail") {
			const item = buildRailItems(state)[state.railIndex];
			if (!item) return state;
			if (item.kind === "all") return clampCursors({ ...state, selectedDomain: undefined });
			if (item.kind === "join_domain") return { ...state, modal: { kind: "join_domain", domainId: "" } };
			return clampCursors({ ...state, selectedDomain: item.domainId });
		}

		if (state.activePane === "sidebar") {
			const item = buildSidebarItems(state)[state.sidebarIndex];
			if (!item) return state;
			if (item.kind === "invite") {
				const peers = buildPeerList(state);
				const idx = peers.findIndex(p => p.node_id === item.entry.node_id);
				return clampCursors({
					...state,
					activePane: "flow",
					specialView: undefined,
					flowIndex: 0,
					selectedMemberId: item.entry.node_id,
					memberIndex: idx >= 0 ? 2 + idx : state.memberIndex,
				});
			}
			if (item.kind === "thread") {
				return {
					...state,
					activePane: "flow",
					specialView: undefined,
					selectedThreadKey: threadKey(item.thread.ref),
					flowIndex: Math.max(0, item.thread.messages.length - 1),
				};
			}
			if (item.kind === "new_thread") {
				return { ...state, modal: { kind: "new_thread", title: "" } };
			}
			return { ...state, activePane: "flow", specialView: item.kind, flowIndex: 0 };
		}

		if (state.activePane === "members") {
			if (state.memberIndex === 0) return { ...state, modal: { kind: "connect", input: "http://" } };
			if (state.memberIndex === 1) {
				void actions.refresh().then(async () => {
					const snap = await actions.snapshot();
					onAsyncUpdate(cur => applySnapshot({ ...cur, notice: { tone: "ok", text: strings.notices.reloaded } }, snap));
				});
				return state;
			}
			const peer = selectedPeer(state);
			if (!peer) return state;
			const res = openComposeForSelection(state, "new");
			return res.state;
		}

		if (state.activePane === "flow") {
			if (state.specialView === "todos") {
				const todo = state.snapshot?.todos[state.flowIndex];
				if (!todo) return state;
				const nextStatus = nextTodoStatus(todo.status);
				void actions.updateTodo(todo.id, nextStatus).then(async () => {
					const snap = await actions.snapshot();
					onAsyncUpdate(cur => applySnapshot({
						...cur,
						notice: { tone: "ok", text: strings.notices.todoUpdated },
					}, snap));
				});
				return state;
			}
			if (state.specialView === "trust") {
				const acq = state.snapshot?.acquisition;
				return {
					...state,
					modal: {
						kind: "acquisition",
						policy: acq?.policy ?? "segment",
						allowText: (acq?.allow ?? []).join(", "),
						field: 0,
					},
				};
			}
			if (!state.specialView) {
				const invited = (state.snapshot?.trust ?? []).filter(t => t.phase === "invited");
				if (state.flowIndex < invited.length) {
					const inv = invited[state.flowIndex]!;
					return {
						...state,
						modal: {
							kind: "trust_confirm_action",
							action: "accept",
							nodeId: inv.node_id,
							fingerprint: inv.fingerprint,
							localFingerprint: inv.local_fingerprint ?? state.snapshot?.fingerprint,
						},
					};
				}
				const thread = activeThread(state);
				const msg = thread?.messages[state.flowIndex - invited.length];
				if (thread && msg) {
					return { ...state, modal: { kind: "message_detail", msg, thread } };
				}
			}
		}
	}

	return state;
}

export async function runOnlyneTui(options: {
	cwd: string;
	client: DaemonClient;
	openBootstrapFirst?: boolean;
}): Promise<void> {
	if (!process.stdin.isTTY || !process.stdout.isTTY) {
		throw new Error("Onlyne TUI requires an interactive TTY terminal.");
	}

	let state = createInitialTuiState();
	try {
		const snap = await options.client.snapshot();
		state = applySnapshot(state, snap);
		if (options.openBootstrapFirst || !snap.configured) {
			state = openBootstrapModal(state, options.cwd, snap.configured);
		}
	} catch (err) {
		state = {
			...state,
			notice: { tone: "err", text: err instanceof Error ? err.message : String(err) },
		};
		if (options.openBootstrapFirst) {
			state = openBootstrapModal(state, options.cwd, true);
		}
	}

	const abort = new AbortController();
	let resolveDone!: () => void;
	const donePromise = new Promise<void>(resolve => {
		resolveDone = resolve;
	});

	const paint = (): void => {
		const width = process.stdout.columns || 120;
		const height = process.stdout.rows || 34;
		const frame = renderTuiFrame(state, width, height);
		process.stdout.write(`\x1b[H\x1b[2J${frame}`);
	};

	const setState = (updater: (cur: TuiState) => TuiState): void => {
		state = updater(state);
		if (state.quitting) {
			abort.abort();
			resolveDone();
			return;
		}
		paint();
	};

	const actions: TuiDaemonActions = {
		refresh: () => options.client.refresh(),
		connect: url => options.client.connect(url),
		send: payload => options.client.send(payload),
		trustBegin: nodeId => options.client.trustBegin(nodeId),
		trustConfirm: (nodeId, fp) => options.client.trustConfirm(nodeId, fp),
		trustAccept: nodeId => options.client.trustAccept(nodeId),
		trustRevoke: nodeId => options.client.trustRevoke(nodeId),
		trustRotate: () => options.client.trustRotate(),
		setAcquisition: (policy, allow) => options.client.setAcquisition(policy, allow),
		setLanguage: lang => options.client.setLanguage(lang),
		domainAction: (op, domainId) => options.client.domainAction(op, domainId),
		createThread: title => options.client.createThread(title),
		updateTodo: (id, status) => options.client.updateTodo(id, status),
		saveConfig: payload => options.client.saveConfig(payload as Parameters<DaemonClient["saveConfig"]>[0]),
		snapshot: () => options.client.snapshot(),
	};

	options.client.subscribe(() => {
		void options.client.snapshot().then(
			snap => {
				if (abort.signal.aborted) return;
				setState(cur => applySnapshot(cur, snap));
			},
			() => undefined,
		);
	}, abort.signal);

	const onData = (chunk: Buffer | string): void => {
		const text = typeof chunk === "string" ? chunk : chunk.toString("utf8");
		const keys = decodeKeyEvents(text);
		for (const key of keys) {
			state = dispatchTuiKey(state, key, options.cwd, actions, setState);
			if (state.quitting) {
				abort.abort();
				resolveDone();
				return;
			}
		}
		paint();
	};

	const onResize = (): void => {
		paint();
	};

	process.stdout.write("\x1b[?1049h\x1b[?25l");
	process.stdin.setRawMode(true);
	process.stdin.resume();
	process.stdin.on("data", onData);
	process.stdout.on("resize", onResize);
	paint();

	try {
		await donePromise;
	} finally {
		abort.abort();
		process.stdin.off("data", onData);
		process.stdout.off("resize", onResize);
		try {
			process.stdin.setRawMode(false);
		} catch {
			// Ignore if stdin already closed
		}
		process.stdin.pause();
		process.stdout.write("\x1b[?25h\x1b[?1049l");
	}
}
