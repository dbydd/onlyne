import { threadKey } from "../core/resolve.ts";
import { shortRegion } from "../config.ts";
import { renderBootstrapLines } from "./bootstrap.ts";
import { ageText, clockText, fitCell, oneLine, padRight, shortAlias, shortId, wrapWidth } from "./format.ts";
import {
	activeThread,
	buildPeerList,
	buildRailItems,
	buildSidebarItems,
	selectedPeer,
	type TuiModal,
	type TuiPane,
	type TuiState,
} from "./state.ts";
import { tuiText } from "./text.ts";

export interface RenderOptions {
	color?: boolean;
}

interface Palette {
	bold: (s: string) => string;
	dim: (s: string) => string;
	cyan: (s: string) => string;
	green: (s: string) => string;
	warn: (s: string) => string;
	err: (s: string) => string;
	rev: (s: string) => string;
}

function makePalette(color: boolean): Palette {
	if (!color) {
		const id = (s: string) => s;
		return { bold: id, dim: id, cyan: id, green: id, warn: id, err: id, rev: id };
	}
	return {
		bold: s => `\x1b[1m${s}\x1b[0m`,
		dim: s => `\x1b[2m${s}\x1b[0m`,
		cyan: s => `\x1b[36m${s}\x1b[0m`,
		green: s => `\x1b[32m${s}\x1b[0m`,
		warn: s => `\x1b[33m${s}\x1b[0m`,
		err: s => `\x1b[31m${s}\x1b[0m`,
		rev: s => `\x1b[7m${s}\x1b[0m`,
	};
}

function renderColumnBox(
	title: string,
	contentLines: string[],
	width: number,
	height: number,
	focused: boolean,
	pal: Palette,
): string[] {
	const w = Math.max(10, width);
	const h = Math.max(4, height);
	const innerW = w - 2;
	const innerH = h - 2;
	const headTitle = fitCell(` ${title} `, innerW);
	const border = focused ? pal.cyan : pal.dim;
	const top = border(`┌${headTitle.replace(/ +$/, m => "─".repeat(m.length))}┐`);
	const bot = border(`└${"─".repeat(innerW)}┘`);

	const rows: string[] = [top];
	for (let i = 0; i < innerH; i += 1) {
		const line = contentLines[i] ?? "";
		const cell = fitCell(line, innerW);
		rows.push(`${border("│")}${cell}${border("│")}`);
	}
	rows.push(bot);
	return rows;
}

function renderRailLines(state: TuiState, innerW: number, pal: Palette): string[] {
	const strings = tuiText(state.locale);
	const items = buildRailItems(state);
	const lines: string[] = [];
	const activePane = state.activePane === "rail";

	if (items.length === 0) {
		lines.push(pal.dim(strings.rail.empty));
		return lines;
	}

	items.forEach((item, idx) => {
		const focused = activePane && state.railIndex === idx;
		let label: string;
		if (item.kind === "all") {
			label = strings.rail.allDomains;
		} else if (item.kind === "join_domain") {
			label = strings.rail.joinDomain;
		} else {
			const selectedMark = state.selectedDomain === item.domainId ? "●" : "○";
			label = `${selectedMark} ${shortId(item.domainId)} (${item.memberCount})`;
		}
		const row = fitCell(label, innerW);
		lines.push(focused ? pal.rev(row) : row);
	});
	return lines;
}

function renderSidebarLines(state: TuiState, innerW: number, pal: Palette): string[] {
	const strings = tuiText(state.locale);
	const items = buildSidebarItems(state);
	const activePane = state.activePane === "sidebar";
	const lines: string[] = [];

	let printedPending = false;
	let printedThreads = false;

	items.forEach((item, idx) => {
		const focused = activePane && state.sidebarIndex === idx;
		if (item.kind === "invite" && !printedPending) {
			printedPending = true;
			lines.push(pal.warn(fitCell(`▼ ${strings.sidebar.pendingSection}`, innerW)));
		}
		if ((item.kind === "thread" || item.kind === "new_thread") && !printedThreads) {
			printedThreads = true;
			lines.push(pal.dim(fitCell(`▼ ${strings.sidebar.threadsSection}`, innerW)));
		}

		let text = "";
		let selected = false;
		if (item.kind === "invite") {
			text = `! ${shortId(item.entry.node_id)}`;
		} else if (item.kind === "thread") {
			const unread = item.thread.unread > 0 ? ` (${item.thread.unread})` : "";
			const ro = item.thread.read_only ? ` [${strings.sidebar.readOnlyTag}]` : "";
			const closed = item.thread.state === "closed" ? ` [${strings.sidebar.closedTag}]` : "";
			text = `${shortAlias(item.thread.alias)}${unread}${ro}${closed}`;
			selected = !state.specialView && state.selectedThreadKey === threadKey(item.thread.ref);
		} else if (item.kind === "new_thread") {
			text = strings.sidebar.newThread;
		} else if (item.kind === "inbox") {
			const count = state.snapshot ? state.snapshot.status.inbox_count : 0;
			text = count > 0 ? `${strings.sidebar.inbox} (${count})` : strings.sidebar.inbox;
			selected = state.specialView === "inbox";
		} else if (item.kind === "trust") {
			text = strings.sidebar.trust;
			selected = state.specialView === "trust";
		} else {
			const todoOpen = (state.snapshot?.todos ?? []).filter(t => t.status !== "completed").length;
			text = todoOpen > 0 ? `${strings.sidebar.todos} (${todoOpen})` : strings.sidebar.todos;
			selected = state.specialView === "todos";
		}

		const cell = fitCell(selected ? `▸ ${text}` : `  ${text}`, innerW);
		if (focused) lines.push(pal.rev(cell));
		else if (selected) lines.push(pal.cyan(cell));
		else lines.push(cell);
	});

	if (!state.snapshot || state.snapshot.threads.length === 0) {
		lines.push(pal.dim(fitCell(strings.sidebar.emptyThreads, innerW)));
	}
	return lines;
}

function renderFlowLines(state: TuiState, innerW: number, innerH: number, pal: Palette): string[] {
	const strings = tuiText(state.locale);
	const snap = state.snapshot;
	if (!snap) return [pal.dim(strings.loading)];
	const lines: string[] = [];
	const activePane = state.activePane === "flow";

	if (state.specialView === "inbox") {
		lines.push(pal.bold(fitCell(strings.flow.inboxTitle, innerW)));
		lines.push("─".repeat(innerW));
		const allRows = [
			...snap.queue.map(q => ({ kind: q.kind, title: q.label, detail: q.detail, at: q.at })),
			...snap.pending_deliveries.map(d => ({
				kind: "wake",
				title: `${d.context_alias} · ${shortId(d.sender)}`,
				detail: `[${d.plan_response}/${d.plan_execution}] ${oneLine(d.text)}`,
				at: d.created_at,
			})),
		];
		if (allRows.length === 0) {
			lines.push(pal.dim(strings.flow.inboxEmpty));
		} else {
			allRows.forEach((row, idx) => {
				const focused = activePane && state.flowIndex === idx;
				const line = fitCell(`[${row.kind}] ${row.title} · ${row.detail} (${ageText(row.at, state.locale)})`, innerW);
				lines.push(focused ? pal.rev(line) : line);
			});
		}
		return lines.slice(0, innerH);
	}

	if (state.specialView === "trust") {
		lines.push(pal.bold(fitCell(strings.flow.trustTitle, innerW)));
		lines.push(`${strings.flow.trustPolicy}: ${snap.acquisition.policy}`);
		lines.push(`${strings.flow.trustAllow}: ${snap.acquisition.allow.join(", ") || "(none)"}`);
		if (snap.fingerprint) {
			lines.push(`${strings.flow.localFp}: ${snap.fingerprint}`);
		}
		lines.push(pal.cyan(`${strings.flow.trustEditRules}   ${strings.flow.trustRotateKey}`));
		lines.push("─".repeat(innerW));
		lines.push(pal.bold(strings.flow.trustAuditTitle));
		if (snap.audit.length === 0) {
			lines.push(pal.dim(strings.flow.trustAuditEmpty));
		} else {
			const auditRows = snap.audit.slice(-Math.max(1, innerH - 7)).reverse();
			auditRows.forEach((entry, idx) => {
				const focused = activePane && state.flowIndex === idx;
				const node = entry.node_id ? ` ${shortId(entry.node_id)}` : "";
				const reason = entry.reason ? ` (${entry.reason})` : "";
				const row = fitCell(`${clockText(entry.at)} [${entry.actor}] ${entry.action}${node}${reason}`, innerW);
				lines.push(focused ? pal.rev(row) : row);
			});
		}
		return lines.slice(0, innerH);
	}

	if (state.specialView === "todos") {
		lines.push(pal.bold(fitCell(strings.flow.todosTitle, innerW)));
		lines.push(pal.dim(fitCell(strings.flow.todoCycleHint, innerW)));
		lines.push("─".repeat(innerW));
		if (snap.todos.length === 0) {
			lines.push(pal.dim(strings.flow.todosEmpty));
		} else {
			snap.todos.forEach((todo, idx) => {
				const focused = activePane && state.flowIndex === idx;
				const badge = todo.status === "completed" ? "[x]" : todo.status === "in_progress" ? "[~]" : todo.status === "blocked" ? "[!]" : "[ ]";
				const row = fitCell(`${badge} ${todo.title} (${todo.phase})`, innerW);
				lines.push(focused ? pal.rev(row) : row);
			});
		}
		return lines.slice(0, innerH);
	}

	const invited = snap.trust.filter(t => t.phase === "invited");
	let cursor = 0;

	if (invited.length > 0) {
		lines.push(pal.warn(fitCell(strings.flow.approvalsTitle, innerW)));
		for (const inv of invited) {
			const focused = activePane && state.flowIndex === cursor;
			cursor += 1;
			const head = fitCell(`! ${shortId(inv.node_id)}  (${ageText(inv.updated_at, state.locale)})`, innerW);
			lines.push(focused ? pal.rev(head) : pal.warn(head));
			lines.push(fitCell(`  ${strings.flow.peerFp}: ${inv.fingerprint}`, innerW));
			if (inv.local_fingerprint) {
				lines.push(fitCell(`  ${strings.flow.localFp}: ${inv.local_fingerprint}`, innerW));
			}
			lines.push(pal.dim(fitCell(`  ${strings.flow.pressAcceptReject}`, innerW)));
		}
		lines.push("─".repeat(innerW));
	}

	const thread = activeThread(state);
	if (!thread) {
		if (invited.length === 0) {
			for (const l of wrapWidth(strings.flow.empty, innerW)) {
				lines.push(pal.dim(l));
			}
		}
		return lines.slice(0, innerH);
	}

	lines.push(pal.bold(fitCell(`${shortAlias(thread.alias)} · ${thread.title} (${shortId(thread.ref.owner)})`, innerW)));
	lines.push(pal.dim(fitCell(strings.flow.messageDetailHint, innerW)));
	lines.push("─".repeat(innerW));

	if (thread.messages.length === 0) {
		lines.push(pal.dim(strings.flow.threadEmpty));
		return lines.slice(0, innerH);
	}

	const msgBlocks: string[][] = [];
	let focusBlockIdx = Math.max(0, state.flowIndex - invited.length);
	if (focusBlockIdx >= thread.messages.length) focusBlockIdx = thread.messages.length - 1;

	thread.messages.forEach((msg, mIdx) => {
		const isFocused = activePane && mIdx === focusBlockIdx;
		const dirArrow = msg.direction === "out" ? "→" : "←";
		const author = shortId(msg.author);
		const head = `${dirArrow} #${msg.seq} ${author}  ${clockText(msg.at)}`;
		const block: string[] = [];
		block.push(isFocused ? pal.rev(fitCell(head, innerW)) : (msg.direction === "out" ? pal.cyan(fitCell(head, innerW)) : pal.bold(fitCell(head, innerW))));
		for (const wl of wrapWidth(msg.text, Math.max(12, innerW - 2))) {
			block.push(fitCell(`  ${wl}`, innerW));
		}
		msgBlocks.push(block);
	});

	const avail = Math.max(4, innerH - lines.length);
	const flatVisible: string[] = [];
	// Keep focused block in view; default anchor from bottom if near end.
	let totalLen = 0;
	for (const b of msgBlocks) totalLen += b.length;
	if (totalLen <= avail) {
		for (const b of msgBlocks) flatVisible.push(...b);
	} else {
		const selectedSlice: string[] = [];
		let startIdx = focusBlockIdx;
		while (startIdx > 0 && selectedSlice.length < Math.floor(avail / 2)) {
			startIdx -= 1;
		}
		for (let i = startIdx; i < msgBlocks.length; i += 1) {
			selectedSlice.push(...msgBlocks[i]!);
			if (selectedSlice.length >= avail) break;
		}
		if (selectedSlice.length < avail) {
			for (let i = startIdx - 1; i >= 0 && selectedSlice.length < avail; i -= 1) {
				selectedSlice.unshift(...msgBlocks[i]!);
			}
		}
		flatVisible.push(...selectedSlice.slice(0, avail));
	}

	lines.push(...flatVisible);
	return lines.slice(0, innerH);
}

function renderMembersLines(state: TuiState, innerW: number, innerH: number, pal: Palette): string[] {
	const strings = tuiText(state.locale);
	const activePane = state.activePane === "members";
	const lines: string[] = [];

	lines.push(activePane && state.memberIndex === 0 ? pal.rev(fitCell(strings.members.connectBtn, innerW)) : pal.cyan(fitCell(strings.members.connectBtn, innerW)));
	lines.push(activePane && state.memberIndex === 1 ? pal.rev(fitCell(strings.members.refreshBtn, innerW)) : pal.dim(fitCell(strings.members.refreshBtn, innerW)));
	lines.push("─".repeat(innerW));

	const peers = buildPeerList(state);
	if (peers.length === 0) {
		lines.push(pal.dim(strings.members.empty));
		return lines;
	}

	const detailReserve = Math.min(8, Math.max(5, Math.floor(innerH * 0.4)));
	const listBudget = Math.max(2, innerH - 3 - detailReserve);
	const selPeer = selectedPeer(state);

	for (let i = 0; i < peers.length && i < listBudget; i += 1) {
		const p = peers[i]!;
		const focused = activePane && state.memberIndex === i + 2;
		const selected = selPeer?.node_id === p.node_id;
		const phase = p.trust ? strings.trustPhase[p.trust.phase] : "new";
		const row = fitCell(`${selected ? "▸" : " "} ${p.label} [${phase}]`, innerW);
		if (focused) lines.push(pal.rev(row));
		else if (selected) lines.push(pal.cyan(row));
		else lines.push(row);
	}

	if (selPeer) {
		lines.push("─".repeat(innerW));
		lines.push(pal.bold(fitCell(`${strings.members.detailTitle}: ${selPeer.label}`, innerW)));
		lines.push(fitCell(`${strings.members.detailRegion}: ${selPeer.region ? shortRegion(selPeer.region) : "-"}`, innerW));
		lines.push(fitCell(`${strings.members.detailUrl}: ${selPeer.base_url ?? "-"}`, innerW));
		lines.push(fitCell(`${strings.members.detailPhase}: ${selPeer.trust ? strings.trustPhase[selPeer.trust.phase] : "untrusted"}`, innerW));
		if (selPeer.trust?.fingerprint) {
			lines.push(fitCell(`${strings.members.detailFp}: ${selPeer.trust.fingerprint}`, innerW));
		}
		lines.push(fitCell(`${strings.members.detailSeen}: ${ageText(selPeer.last_seen, state.locale)}`, innerW));
		for (const h of wrapWidth(strings.members.detailActions, innerW)) {
			lines.push(pal.dim(h));
		}
	}

	return lines.slice(0, innerH);
}

function renderModalOverlay(
	baseLines: string[],
	modal: TuiModal,
	state: TuiState,
	width: number,
	height: number,
	pal: Palette,
): string[] {
	const strings = tuiText(state.locale);
	const boxW = Math.max(40, Math.min(width - 6, modal.kind === "help" || modal.kind === "compose" || modal.kind === "message_detail" ? 78 : 66));
	const innerW = boxW - 4;
	const body: string[] = [];
	let title = "";

	if (modal.kind === "help") {
		title = strings.modals.helpTitle;
		body.push(...strings.modals.helpLines.flatMap(l => wrapWidth(l, innerW)));
	} else if (modal.kind === "connect") {
		title = strings.modals.connectTitle;
		body.push(...wrapWidth(strings.modals.connectPrompt, innerW));
		body.push("");
		body.push(pal.rev(fitCell(` ${modal.input}▏`, innerW)));
	} else if (modal.kind === "new_thread") {
		title = strings.modals.newThreadTitle;
		body.push(...wrapWidth(strings.modals.newThreadPrompt, innerW));
		body.push("");
		body.push(pal.rev(fitCell(` ${modal.title}▏`, innerW)));
	} else if (modal.kind === "join_domain") {
		title = strings.modals.joinDomainTitle;
		body.push(...wrapWidth(strings.modals.joinDomainPrompt, innerW));
		body.push("");
		body.push(pal.rev(fitCell(` ${modal.domainId}▏`, innerW)));
	} else if (modal.kind === "compose") {
		title = strings.modals.composeTitle;
		if (modal.replyTo) {
			body.push(`${strings.modals.composeReplyTo}: ${modal.replyTo.author} (${modal.replyTo.id})`);
		}
		if (modal.to) {
			body.push(`${strings.modals.composeTo}: ${shortId(modal.to)}`);
		}
		if (modal.thread) {
			body.push(`${strings.modals.composeThread}: ${modal.thread.id} (${shortId(modal.thread.owner)})`);
		}
		body.push(`${strings.modals.composeMode}: [${modal.response}]`);
		body.push("─".repeat(innerW));
		const textLines = modal.text.split("\n");
		const maxLines = 8;
		const visible = textLines.slice(-maxLines);
		visible.forEach((line, idx) => {
			const suffix = idx === visible.length - 1 ? "▏" : "";
			for (const w of wrapWidth(`${line}${suffix}`, innerW)) {
				body.push(pal.cyan(w));
			}
		});
		body.push("─".repeat(innerW));
		body.push(pal.dim(fitCell(strings.modals.composeHint, innerW)));
	} else if (modal.kind === "exchange") {
		title = strings.modals.exchangeTitle;
		body.push(`${strings.members.detailTitle}: ${shortId(modal.nodeId)} (${modal.baseUrl})`);
		body.push("");
		body.push(`${strings.flow.localFp}: ${modal.localFingerprint}`);
		body.push(`${strings.flow.peerFp}: ${modal.fingerprint}`);
		body.push("");
		for (const l of wrapWidth(strings.modals.exchangeWarn, innerW)) {
			body.push(pal.warn(l));
		}
		body.push(pal.cyan(fitCell(strings.modals.exchangeAction, innerW)));
	} else if (modal.kind === "trust_confirm_action") {
		title = modal.action === "accept" ? strings.modals.confirmAcceptTitle : strings.modals.confirmRevokeTitle;
		body.push(`${strings.members.detailTitle}: ${shortId(modal.nodeId)}`);
		body.push(`${strings.flow.peerFp}: ${modal.fingerprint}`);
		if (modal.localFingerprint) {
			body.push(`${strings.flow.localFp}: ${modal.localFingerprint}`);
		}
		body.push("");
		body.push(pal.warn(fitCell(strings.modals.confirmPrompt, innerW)));
	} else if (modal.kind === "acquisition") {
		title = strings.modals.acqTitle;
		const pLine = `${strings.modals.acqPolicy}: [${modal.policy}]`;
		const aLine = `${strings.modals.acqAllow}: ${modal.allowText}${modal.field === 1 ? "▏" : ""}`;
		body.push(modal.field === 0 ? pal.rev(fitCell(pLine, innerW)) : fitCell(pLine, innerW));
		body.push(modal.field === 1 ? pal.rev(fitCell(aLine, innerW)) : fitCell(aLine, innerW));
		body.push("");
		body.push(pal.dim(fitCell(strings.modals.acqHint, innerW)));
	} else if (modal.kind === "message_detail") {
		title = strings.modals.msgDetailTitle;
		body.push(pal.bold(`#${modal.msg.seq} · ${shortId(modal.msg.author)} · ${clockText(modal.msg.at)}`));
		body.push("─".repeat(innerW));
		for (const w of wrapWidth(modal.msg.text, innerW)) {
			body.push(w);
		}
		body.push("─".repeat(innerW));
		body.push(pal.cyan(fitCell(strings.modals.msgDetailActions, innerW)));
	}

	const boxH = Math.min(height - 2, Math.max(6, body.length + 4));
	const modalLines: string[] = [];
	const topBar = `╔${fitCell(` ${title} `, boxW - 2).replace(/ +$/, m => "═".repeat(m.length))}╗`;
	modalLines.push(pal.cyan(topBar));
	for (let i = 0; i < boxH - 2; i += 1) {
		const rowText = i === 0 || i === boxH - 3 ? "" : body[i - 1] ?? "";
		modalLines.push(`${pal.cyan("║")} ${fitCell(rowText, boxW - 4)} ${pal.cyan("║")}`);
	}
	modalLines.push(pal.cyan(`╚${"═".repeat(boxW - 2)}╝`));

	const startY = Math.max(1, Math.floor((height - modalLines.length) / 2));
	const startX = Math.max(1, Math.floor((width - boxW) / 2));

	const out = [...baseLines];
	for (let i = 0; i < modalLines.length; i += 1) {
		const targetY = startY + i;
		if (targetY >= out.length) break;
		// Replace whole centered line cleanly with subtle padding margins
		out[targetY] = `${" ".repeat(startX)}${modalLines[i]!}`;
	}
	return out;
}

export function renderTuiFrame(
	state: TuiState,
	width: number,
	height: number,
	options: RenderOptions = {},
): string {
	const w = Math.max(60, width);
	const h = Math.max(18, height);
	const color = options.color ?? (process.env.NO_COLOR === undefined);
	const pal = makePalette(color);
	const strings = tuiText(state.locale);

	if (state.bootstrap) {
		const lines = renderBootstrapLines(state.bootstrap, w, h, state.locale, color);
		return lines.join("\n");
	}

	const snap = state.snapshot;
	const headerText = snap
		? `Onlyne · ${shortId(snap.status.node_id)} · ${shortRegion(snap.status.region)} · trusted ${snap.status.trusted} · pending ${snap.status.pending_approvals} · judge ${snap.judge_mode}`
		: strings.appTitle;

	const headerLine = pal.bold(pal.cyan(fitCell(headerText, w)));
	const bannerLine = snap && !snap.configured
		? pal.warn(fitCell(strings.unconfiguredBanner, w))
		: state.notice
			? (state.notice.tone === "err" ? pal.err : state.notice.tone === "ok" ? pal.green : pal.cyan)(fitCell(state.notice.text, w))
			: pal.dim(fitCell(snap?.status.endpoint ?? strings.loading, w));

	const bodyH = h - 4;
	const frameLines: string[] = [headerLine, bannerLine];

	if (w >= 96) {
		const railW = Math.max(14, Math.min(20, Math.floor(w * 0.14)));
		const sideW = Math.max(20, Math.min(28, Math.floor(w * 0.22)));
		const memW = Math.max(22, Math.min(30, Math.floor(w * 0.23)));
		const flowW = Math.max(28, w - railW - sideW - memW);

		const railCol = renderColumnBox(strings.columns.rail, renderRailLines(state, railW - 2, pal), railW, bodyH, state.activePane === "rail", pal);
		const sideCol = renderColumnBox(strings.columns.sidebar, renderSidebarLines(state, sideW - 2, pal), sideW, bodyH, state.activePane === "sidebar", pal);
		const flowCol = renderColumnBox(strings.columns.flow, renderFlowLines(state, flowW - 2, bodyH - 2, pal), flowW, bodyH, state.activePane === "flow", pal);
		const memCol = renderColumnBox(strings.columns.members, renderMembersLines(state, memW - 2, bodyH - 2, pal), memW, bodyH, state.activePane === "members", pal);

		for (let i = 0; i < bodyH; i += 1) {
			frameLines.push(`${railCol[i] ?? ""}${sideCol[i] ?? ""}${flowCol[i] ?? ""}${memCol[i] ?? ""}`);
		}
	} else {
		const panes: Array<[TuiPane, string]> = [
			["rail", `1:${strings.columns.rail}`],
			["sidebar", `2:${strings.columns.sidebar}`],
			["flow", `3:${strings.columns.flow}`],
			["members", `4:${strings.columns.members}`],
		];
		const tabLine = panes
			.map(([p, label]) => (state.activePane === p ? pal.rev(` ${label} `) : pal.dim(` ${label} `)))
			.join(" ");
		frameLines.push(fitCell(tabLine, w));
		const innerH = bodyH - 3;
		const lines =
			state.activePane === "rail"
				? renderRailLines(state, w - 2, pal)
				: state.activePane === "sidebar"
					? renderSidebarLines(state, w - 2, pal)
					: state.activePane === "flow"
						? renderFlowLines(state, w - 2, innerH, pal)
						: renderMembersLines(state, w - 2, innerH, pal);
		const col = renderColumnBox(strings.columns[state.activePane], lines, w, bodyH - 1, true, pal);
		frameLines.push(...col);
	}

	const hints = w >= 96 ? strings.footer.hintsWide : strings.footer.hintsCompact;
	const footer = `${hints} | ${strings.footer.langLabel}  ${strings.footer.initLabel}  ${strings.footer.helpLabel}  ${strings.footer.quitLabel}`;
	frameLines.push(pal.dim(padRight(fitCell(footer, w), w)));

	const finalized = state.modal
		? renderModalOverlay(frameLines, state.modal, state, w, h, pal)
		: frameLines;
	return finalized.slice(0, h).join("\n");
}
