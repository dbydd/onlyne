import type { AcquirePolicy } from "@onlyne/node-host/acquire";
import type { Snapshot, TrustSnapshotEntry, ViewerDiscovery, ViewerThread } from "../core/types.ts";
import { createGuiTransport, type GuiTransport } from "./client.ts";

type MainView = "thread" | "inbox" | "trust" | "todos" | "setup";
type MobilePane = "channels" | "main" | "members";

interface UiState {
	snapshot?: Snapshot;
	selectedDomain: string | "all";
	mainView: MainView;
	mobilePane: MobilePane;
	selectedThreadKey?: string;
	selectedPeerId?: string;
	replyTarget?: { sender: string; id: string; seq: string; author: string; threadKey: string };
	pendingExchange?: { node_id: string; fingerprint: string; base_url: string };
	seedInput: string;
	composerText: string;
	composerKind: string;
	composerResponse: "async" | "await" | "none";
	composerTarget: string;
	newThreadTitle: string;
	showNewThread: boolean;
	policyDraft?: AcquirePolicy;
	allowDraft?: string;
	setupDraft?: {
		region: string;
		node_id: string;
		bind: string;
		port: string;
		key_scope: "project" | "global" | "none";
		page_size: string;
		seeds: string;
		policy: AcquirePolicy;
		allow: string;
		judge_mode: "auto" | "sampling" | "openai" | "gemini" | "agy-cli" | "none";
		judge_model: string;
		judge_base_url: string;
	};
	banner?: { kind: "info" | "error"; text: string };
	busy: boolean;
}

const TEXT = {
	en: {
		all: "All",
		channels: "Threads",
		viewMain: "View",
		peers: "Peers",
		pendingTrust: "Pending Trust",
		threads: "Threads",
		inbox: "Inbox",
		trustRules: "Trust & Policy",
		todos: "Todos",
		setup: "Node Setup",
		rotate: "Rotate Key",
		refresh: "Refresh",
		connectSeed: "Connect seed URL...",
		connect: "Connect",
		newThread: "+ New Thread",
		create: "Create",
		cancel: "Cancel",
		send: "Send",
		replyingTo: "Replying to",
		clear: "Clear",
		target: "Target",
		auto: "(auto)",
		kind: "Kind",
		plan: "Plan",
		pinContext: "Pin Context",
		askAgent: "Ask Antigravity",
		approveTrust: "Confirm & Approve",
		rejectTrust: "Reject",
		beginExchange: "Begin Exchange",
		confirmFingerprint: "Confirm Fingerprint",
		accept: "Accept",
		revoke: "Revoke",
		composeToPeer: "Message Peer",
		policy: "Acquisition Policy",
		allow: "Allow CIDRs / Hosts (comma or newline)",
		savePolicy: "Save Policy",
		saveConfig: "Save & Start Node",
		auditLog: "Acquisition Audit",
		emptyThread: "No messages in this thread yet.",
		selectPrompt: "Select a thread or peer to inspect.",
		unconfigured: "Onlyne is not configured yet in this workspace. Fill out the Node Setup form to start your decentralized node.",
		fpLabel: "Fingerprint",
		nodeLabel: "Node ID",
		urlLabel: "Base URL",
		phaseLabel: "Phase",
		seenLabel: "Last Seen",
		sourceLabel: "Source",
		verifyPrompt: "Verify remote fingerprint matches the peer before confirming:",
	},
	"zh-CN": {
		all: "全部",
		channels: "话题",
		viewMain: "视图",
		peers: "节点",
		pendingTrust: "待确认信任",
		threads: "话题列表",
		inbox: "收件箱",
		trustRules: "信任与准入策略",
		todos: "待办",
		setup: "节点配置",
		rotate: "轮换密钥",
		refresh: "刷新",
		connectSeed: "输入 Seed URL 连接...",
		connect: "连接",
		newThread: "+ 新话题",
		create: "创建",
		cancel: "取消",
		send: "发送",
		replyingTo: "回复消息",
		clear: "取消",
		target: "目标",
		auto: "(自动)",
		kind: "类型",
		plan: "响应计划",
		pinContext: "写入上下文",
		askAgent: "发送给 Agent",
		approveTrust: "核对并批准",
		rejectTrust: "拒绝",
		beginExchange: "发起密钥交换",
		confirmFingerprint: "确认指纹无误",
		accept: "接受",
		revoke: "撤销",
		composeToPeer: "发消息",
		policy: "自动准入策略",
		allow: "允许网段 / 主机（逗号或换行分隔）",
		savePolicy: "保存策略",
		saveConfig: "保存并启动节点",
		auditLog: "准入审计记录",
		emptyThread: "该话题暂无消息。",
		selectPrompt: "请选择一个话题或对端节点。",
		unconfigured: "当前工作区尚未初始化 Onlyne 配置。请填写下方节点设置并启动节点。",
		fpLabel: "公钥指纹",
		nodeLabel: "节点 ID",
		urlLabel: "服务地址",
		phaseLabel: "信任阶段",
		seenLabel: "最近发现",
		sourceLabel: "发现来源",
		verifyPrompt: "请核对以下远端公钥指纹后再确认交换：",
	},
} as const;

function tr(lang: "en" | "zh-CN", key: keyof typeof TEXT.en): string {
	return TEXT[lang][key];
}

function escapeHtml(raw: string): string {
	return raw
		.replace(/&/g, "&amp;")
		.replace(/</g, "&lt;")
		.replace(/>/g, "&gt;")
		.replace(/"/g, "&quot;")
		.replace(/'/g, "&#39;");
}

function shortId(nodeId: string): string {
	const tail = nodeId.split(":").at(-1) ?? nodeId;
	try {
		return decodeURIComponent(tail);
	} catch {
		return tail;
	}
}

function shortRegionName(region: string): string {
	const prefix = "urn:onlyne:region:";
	return region.startsWith(prefix) ? decodeURIComponent(region.slice(prefix.length)) : region;
}

function threadKey(thread: Pick<ViewerThread, "ref">): string {
	return `${thread.ref.owner}/${thread.ref.id}`;
}

function formatClock(at?: string): string {
	if (!at) return "";
	const ms = Date.parse(at);
	if (!Number.isFinite(ms)) return at;
	return new Date(ms).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
}

function findPeerInfo(snapshot: Snapshot, nodeId: string): {
	trust?: TrustSnapshotEntry;
	discovery?: ViewerDiscovery;
} {
	return {
		trust: snapshot.trust.find(t => t.node_id === nodeId),
		discovery: snapshot.discovery.find(d => d.node_id === nodeId),
	};
}

export async function mountOnlyneGui(root: HTMLElement): Promise<void> {
	let transport!: GuiTransport;
	const state: UiState = {
		selectedDomain: "all",
		mainView: "thread",
		mobilePane: "main",
		seedInput: "",
		composerText: "",
		composerKind: "note",
		composerResponse: "async",
		composerTarget: "",
		newThreadTitle: "",
		showNewThread: false,
		busy: false,
	};

	let deferredRender = false;

	const isEditingInput = (): boolean => {
		const active = document.activeElement as HTMLElement | null;
		if (!active || !root.contains(active)) return false;
		const tag = active.tagName;
		return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT";
	};

	const showBanner = (kind: "info" | "error", text: string): void => {
		state.banner = { kind, text };
		render();
	};

	const refresh = async (fromBackground = false): Promise<void> => {
		const snap = await transport.snapshot();
		const wasUnconfigured = state.snapshot === undefined && !snap.configured;
		state.snapshot = snap;
		if (wasUnconfigured && !snap.configured) {
			state.mainView = "setup";
		} else if (state.mainView === "setup" && snap.configured && state.setupDraft === undefined) {
			state.mainView = "thread";
		}
		if (state.selectedThreadKey === undefined && snap.threads.length > 0) {
			state.selectedThreadKey = threadKey(snap.threads[0]!);
		} else if (state.selectedThreadKey !== undefined && !snap.threads.some(t => threadKey(t) === state.selectedThreadKey)) {
			state.selectedThreadKey = snap.threads[0] ? threadKey(snap.threads[0]) : undefined;
		}
		if (state.policyDraft === undefined) {
			state.policyDraft = snap.acquisition.policy;
			state.allowDraft = snap.acquisition.allow.join("\n");
		}
		if (fromBackground && isEditingInput()) {
			deferredRender = true;
			return;
		}
		deferredRender = false;
		render();
	};

	const runAction = async (name: string, payload?: Record<string, unknown>, okMsg?: string): Promise<unknown> => {
		state.busy = true;
		state.banner = undefined;
		render();
		try {
			const res = await transport.action(name, payload);
			await refresh();
			if (okMsg) showBanner("info", okMsg);
			return res;
		} catch (err) {
			showBanner("error", err instanceof Error ? err.message : String(err));
			return undefined;
		} finally {
			state.busy = false;
			render();
		}
	};

	const initSetupDraft = (snap: Snapshot): NonNullable<UiState["setupDraft"]> => {
		if (state.setupDraft !== undefined) return state.setupDraft;
		const draft: NonNullable<UiState["setupDraft"]> = {
			region: shortRegionName(snap.status.region || "local"),
			node_id: shortId(snap.status.node_id || "agy-node"),
			bind: "127.0.0.1",
			port: "",
			key_scope: "project",
			page_size: "50",
			seeds: "",
			policy: snap.acquisition.policy,
			allow: snap.acquisition.allow.join(", "),
			judge_mode: (snap.judge_mode as NonNullable<UiState["setupDraft"]>["judge_mode"]) || "auto",
			judge_model: "",
			judge_base_url: "",
		};
		state.setupDraft = draft;
		return draft;
	};

	const render = (): void => {
		const snap = state.snapshot;
		if (!snap) {
			root.innerHTML = `<div class="empty-state">Loading Onlyne Node...</div>`;
			return;
		}
		const lang = snap.language;
		const caps = transport.hostActions();
		const pendingInvites = snap.trust.filter(t => t.phase === "invited");
		const selectedThread = snap.threads.find(t => threadKey(t) === state.selectedThreadKey);
		const allPeerIds = [
			...new Set([
				...snap.trust.map(t => t.node_id),
				...snap.discovery.map(d => d.node_id),
			]),
		].filter(id => id !== snap.status.node_id);

		if (state.selectedPeerId === undefined && allPeerIds.length > 0) {
			state.selectedPeerId = pendingInvites[0]?.node_id ?? allPeerIds[0];
		}
		const selectedPeer = state.selectedPeerId ? findPeerInfo(snap, state.selectedPeerId) : undefined;
		const activeRequestPeer = pendingInvites.find(t => t.node_id === state.selectedPeerId) ?? pendingInvites[0];

		const railHtml = `
			<aside class="rail">
				<button class="rail-btn ${state.selectedDomain === "all" && state.mainView !== "setup" ? "active" : ""}" data-act="select-domain" data-domain="all" title="${escapeHtml(tr(lang, "all"))}">
					ALL
					${snap.status.unread > 0 ? `<span class="badge">${snap.status.unread}</span>` : ""}
				</button>
				${snap.domains.map(d => `
					<button class="rail-btn ${state.selectedDomain === d.domain_id ? "active" : ""}" data-act="select-domain" data-domain="${escapeHtml(d.domain_id)}" title="${escapeHtml(d.domain_id)}">
						${escapeHtml(d.domain_id.slice(0, 2).toUpperCase())}
					</button>
				`).join("")}
				<div class="rail-spacer"></div>
				<button class="rail-btn" data-act="toggle-lang" title="EN / 中文">${lang === "zh-CN" ? "中" : "EN"}</button>
				<button class="rail-btn ${state.mainView === "setup" ? "active" : ""}" data-act="open-view" data-view="setup" title="${escapeHtml(tr(lang, "setup"))}">⚙</button>
				<button class="rail-btn" data-act="refresh" title="${escapeHtml(tr(lang, "refresh"))}">↻</button>
			</aside>
		`;

		const sidebarHtml = `
			<aside class="channels">
				<div class="panel-head">
					<div class="panel-title-row">
						<h2>${escapeHtml(shortRegionName(snap.status.region))}</h2>
						<span class="status-pill">
							<span class="dot ${snap.configured ? "ok" : "warn"}"></span>
							${escapeHtml(shortId(snap.status.node_id))}
						</span>
					</div>
					<div class="panel-sub">
						<span>${snap.fingerprint ? `fp ${escapeHtml(snap.fingerprint)}` : "no key"}</span>
						${snap.fingerprint ? `<button class="copy-chip" data-act="copy" data-copy="${escapeHtml(snap.fingerprint)}">copy</button>` : ""}
					</div>
					<div class="panel-sub">
						<span title="${escapeHtml(snap.status.endpoint)}">${escapeHtml(snap.status.endpoint.replace(/^https?:\/\//, ""))}</span>
					</div>
				</div>
				<div class="sidebar-scroll">
					${pendingInvites.length > 0 ? `
						<div class="section-label">${escapeHtml(tr(lang, "pendingTrust"))}</div>
						${pendingInvites.map(p => `
							<button class="row-item ${state.selectedPeerId === p.node_id ? "active" : ""}" data-act="select-peer" data-peer="${escapeHtml(p.node_id)}">
								<span class="dot warn"></span>
								<span class="name">${escapeHtml(shortId(p.node_id))}</span>
								<span class="count alert">!</span>
							</button>
						`).join("")}
					` : ""}
					<div class="section-label">
						<span>${escapeHtml(tr(lang, "threads"))}</span>
						<button data-act="toggle-new-thread">+</button>
					</div>
					${state.showNewThread ? `
						<div style="padding:4px 4px 8px;display:flex;gap:4px;">
							<input id="new-thread-title" placeholder="Thread title..." value="${escapeHtml(state.newThreadTitle)}" style="flex:1;min-width:0;padding:4px 6px;border:1px solid var(--line);border-radius:5px;background:var(--surface-2);font-size:12px;" />
							<button class="btn primary" data-act="create-thread">${escapeHtml(tr(lang, "create"))}</button>
						</div>
					` : ""}
					${snap.threads.map(t => {
						const k = threadKey(t);
						const active = state.mainView === "thread" && state.selectedThreadKey === k;
						return `
							<button class="row-item ${active ? "active" : ""}" data-act="select-thread" data-thread="${escapeHtml(k)}">
								<span class="muted">#</span>
								<span class="name">${escapeHtml(t.title)}</span>
								${t.unread > 0 ? `<span class="count alert">${t.unread}</span>` : ""}
							</button>
						`;
					}).join("")}
					<div class="section-label">Views</div>
					<button class="row-item ${state.mainView === "inbox" ? "active" : ""}" data-act="open-view" data-view="inbox">
						<span class="name">${escapeHtml(tr(lang, "inbox"))}</span>
						${snap.queue.length > 0 ? `<span class="count alert">${snap.queue.length}</span>` : ""}
					</button>
					<button class="row-item ${state.mainView === "trust" ? "active" : ""}" data-act="open-view" data-view="trust">
						<span class="name">${escapeHtml(tr(lang, "trustRules"))}</span>
					</button>
					<button class="row-item ${state.mainView === "todos" ? "active" : ""}" data-act="open-view" data-view="todos">
						<span class="name">${escapeHtml(tr(lang, "todos"))}</span>
						${snap.todos.length > 0 ? `<span class="count">${snap.todos.length}</span>` : ""}
					</button>
					<button class="row-item ${state.mainView === "setup" ? "active" : ""}" data-act="open-view" data-view="setup">
						<span class="name">${escapeHtml(tr(lang, "setup"))}</span>
					</button>
				</div>
			</aside>
		`;

		const requestBarHtml = state.pendingExchange ? `
			<div class="requestbar">
				<div>
					<strong>${escapeHtml(tr(lang, "verifyPrompt"))}</strong>
					<code>${escapeHtml(shortId(state.pendingExchange.node_id))}</code>
					<code>${escapeHtml(state.pendingExchange.fingerprint)}</code>
				</div>
				<div class="requestbar-actions">
					<button class="btn primary" ${state.busy ? "disabled" : ""} data-act="trust-confirm" data-node="${escapeHtml(state.pendingExchange.node_id)}" data-fp="${escapeHtml(state.pendingExchange.fingerprint)}">${escapeHtml(tr(lang, "confirmFingerprint"))}</button>
					<button class="btn" data-act="clear-exchange">${escapeHtml(tr(lang, "cancel"))}</button>
				</div>
			</div>
		` : activeRequestPeer ? `
			<div class="requestbar">
				<div>
					<strong>Trust Invitation:</strong>
					<code>${escapeHtml(shortId(activeRequestPeer.node_id))}</code>
					· fp <code>${escapeHtml(activeRequestPeer.fingerprint)}</code>
					${activeRequestPeer.url ? `· <code>${escapeHtml(activeRequestPeer.url)}</code>` : ""}
				</div>
				<div class="requestbar-actions">
					<button class="btn primary" ${state.busy ? "disabled" : ""} data-act="trust-accept" data-node="${escapeHtml(activeRequestPeer.node_id)}">${escapeHtml(tr(lang, "approveTrust"))}</button>
					<button class="btn danger" ${state.busy ? "disabled" : ""} data-act="trust-revoke" data-node="${escapeHtml(activeRequestPeer.node_id)}">${escapeHtml(tr(lang, "rejectTrust"))}</button>
				</div>
			</div>
		` : "";

		const bannerHtml = state.banner ? `
			<div class="banner ${state.banner.kind === "error" ? "error" : ""}">
				<span>${escapeHtml(state.banner.text)}</span>
				<button class="copy-chip" data-act="clear-banner">×</button>
			</div>
		` : "";

		let mainHeadTitle = "Onlyne";
		let mainHeadSub = snap.status.endpoint;
		let mainBodyHtml = "";
		let composerHtml = "";

		if (state.mainView === "setup") {
			const draft = initSetupDraft(snap);
			mainHeadTitle = tr(lang, "setup");
			mainHeadSub = `${snap.workspace}/.onlyne/config.toml`;
			mainBodyHtml = `
				${!snap.configured ? `<div class="banner">${escapeHtml(tr(lang, "unconfigured"))}</div>` : ""}
				<div class="card">
					<h3>${escapeHtml(tr(lang, "setup"))}</h3>
					<div class="form-grid">
						<div class="field"><label>Region</label><input id="cfg-region" value="${escapeHtml(draft.region)}" /></div>
						<div class="field"><label>Node ID</label><input id="cfg-node" value="${escapeHtml(draft.node_id)}" /></div>
						<div class="field"><label>Server Bind</label><input id="cfg-bind" value="${escapeHtml(draft.bind)}" /></div>
						<div class="field"><label>Port (empty = ephemeral)</label><input id="cfg-port" value="${escapeHtml(draft.port)}" /></div>
						<div class="field">
							<label>Key Scope</label>
							<select id="cfg-scope">
								<option value="project" ${draft.key_scope === "project" ? "selected" : ""}>project</option>
								<option value="global" ${draft.key_scope === "global" ? "selected" : ""}>global</option>
								<option value="none" ${draft.key_scope === "none" ? "selected" : ""}>none</option>
							</select>
						</div>
						<div class="field"><label>Page Size</label><input id="cfg-page" value="${escapeHtml(draft.page_size)}" /></div>
						<div class="field"><label>Discovery Seeds (comma separated)</label><input id="cfg-seeds" value="${escapeHtml(draft.seeds)}" placeholder="http://127.0.0.1:57777/onlyne/v3" /></div>
						<div class="field">
							<label>Acquisition Policy</label>
							<select id="cfg-policy">
								<option value="segment" ${draft.policy === "segment" ? "selected" : ""}>segment</option>
								<option value="all" ${draft.policy === "all" ? "selected" : ""}>all</option>
								<option value="none" ${draft.policy === "none" ? "selected" : ""}>none</option>
							</select>
						</div>
						<div class="field"><label>Allow CIDRs / Hosts</label><input id="cfg-allow" value="${escapeHtml(draft.allow)}" /></div>
						<div class="field">
							<label>SystemOne Judge Mode</label>
							<select id="cfg-judge-mode">
								<option value="auto" ${draft.judge_mode === "auto" ? "selected" : ""}>auto</option>
								<option value="sampling" ${draft.judge_mode === "sampling" ? "selected" : ""}>sampling</option>
								<option value="openai" ${draft.judge_mode === "openai" ? "selected" : ""}>openai</option>
								<option value="gemini" ${draft.judge_mode === "gemini" ? "selected" : ""}>gemini</option>
								<option value="agy-cli" ${draft.judge_mode === "agy-cli" ? "selected" : ""}>agy-cli</option>
								<option value="none" ${draft.judge_mode === "none" ? "selected" : ""}>none</option>
							</select>
						</div>
						<div class="field"><label>Judge Model (optional)</label><input id="cfg-judge-model" value="${escapeHtml(draft.judge_model)}" /></div>
						<div class="field"><label>Judge Base URL (optional)</label><input id="cfg-judge-base" value="${escapeHtml(draft.judge_base_url)}" /></div>
					</div>
					<div>
						<button class="btn primary" ${state.busy ? "disabled" : ""} data-act="save-config">${escapeHtml(tr(lang, "saveConfig"))}</button>
					</div>
				</div>
			`;
		} else if (state.mainView === "inbox") {
			mainHeadTitle = tr(lang, "inbox");
			mainHeadSub = `${snap.queue.length} queued items`;
			mainBodyHtml = snap.queue.length === 0
				? `<div class="empty-state">Inbox is empty.</div>`
				: snap.queue.map(item => `
					<div class="msg">
						<div class="msg-head">
							<div class="msg-author-wrap">
								<span class="pill">${escapeHtml(item.kind)}</span>
								<span class="msg-author">${escapeHtml(item.label)}</span>
							</div>
							<span class="msg-meta">${escapeHtml(formatClock(item.at))}</span>
						</div>
						<div class="msg-body">${escapeHtml(item.detail)}</div>
					</div>
				`).join("");
		} else if (state.mainView === "trust") {
			mainHeadTitle = tr(lang, "trustRules");
			mainHeadSub = `policy=${snap.acquisition.policy} · allow_loopback=${snap.acquisition.allow_loopback}`;
			mainBodyHtml = `
				<div class="card">
					<h3>${escapeHtml(tr(lang, "policy"))}</h3>
					<div class="form-grid">
						<div class="field">
							<label>${escapeHtml(tr(lang, "policy"))}</label>
							<select id="policy-select">
								<option value="all" ${(state.policyDraft ?? snap.acquisition.policy) === "all" ? "selected" : ""}>all</option>
								<option value="segment" ${(state.policyDraft ?? snap.acquisition.policy) === "segment" ? "selected" : ""}>segment</option>
								<option value="none" ${(state.policyDraft ?? snap.acquisition.policy) === "none" ? "selected" : ""}>none</option>
							</select>
						</div>
						<div class="field">
							<label>${escapeHtml(tr(lang, "allow"))}</label>
							<textarea id="policy-allow" rows="3">${escapeHtml(state.allowDraft ?? snap.acquisition.allow.join("\n"))}</textarea>
						</div>
					</div>
					<div class="detail-actions">
						<button class="btn primary" ${state.busy ? "disabled" : ""} data-act="save-policy">${escapeHtml(tr(lang, "savePolicy"))}</button>
						<button class="btn" ${state.busy ? "disabled" : ""} data-act="trust-rotate">${escapeHtml(tr(lang, "rotate"))}</button>
					</div>
				</div>
				<div class="card">
					<h3>${escapeHtml(tr(lang, "auditLog"))}</h3>
					<div class="audit-list">
						${snap.audit.length === 0 ? `<div class="muted">No audit entries recorded yet.</div>` : snap.audit.map(entry => `
							<div class="audit-row">
								<div>
									<span class="pill">${escapeHtml(entry.actor)}</span>
									<strong>${escapeHtml(entry.action)}</strong>
									${entry.node_id ? `<code>${escapeHtml(shortId(entry.node_id))}</code>` : ""}
									${entry.reason ? `<span class="muted">(${escapeHtml(entry.reason)})</span>` : ""}
								</div>
								<span class="muted">${escapeHtml(formatClock(entry.at))}</span>
							</div>
						`).join("")}
					</div>
				</div>
			`;
		} else if (state.mainView === "todos") {
			mainHeadTitle = tr(lang, "todos");
			mainHeadSub = `${snap.todos.length} Onlyne items`;
			mainBodyHtml = snap.todos.length === 0
				? `<div class="empty-state">No Onlyne todo items recorded.</div>`
				: snap.todos.map(td => `
					<div class="msg">
						<div class="msg-head">
							<div class="msg-author-wrap">
								<span class="pill">${escapeHtml(td.phase)}</span>
								<span class="msg-author">${escapeHtml(td.title)}</span>
							</div>
							<select data-act="todo-status" data-id="${escapeHtml(td.id)}" style="font-size:11px;padding:2px 6px;border:1px solid var(--line);border-radius:4px;">
								<option value="pending" ${td.status === "pending" ? "selected" : ""}>pending</option>
								<option value="in_progress" ${td.status === "in_progress" ? "selected" : ""}>in_progress</option>
								<option value="completed" ${td.status === "completed" ? "selected" : ""}>completed</option>
								<option value="blocked" ${td.status === "blocked" ? "selected" : ""}>blocked</option>
							</select>
						</div>
						${td.explanation ? `<div class="msg-body">${escapeHtml(td.explanation)}</div>` : ""}
					</div>
				`).join("");
		} else if (selectedThread) {
			const tk = threadKey(selectedThread);
			mainHeadTitle = `# ${selectedThread.title}`;
			mainHeadSub = `${tk} · ${selectedThread.state}${selectedThread.read_only ? " · read-only" : ""}`;
			mainBodyHtml = selectedThread.messages.length === 0
				? `<div class="empty-state">${escapeHtml(tr(lang, "emptyThread"))}</div>`
				: selectedThread.messages.map(m => {
					const isSelected = state.replyTarget?.id === m.ref.id && state.replyTarget?.sender === m.ref.sender;
					return `
						<div class="msg ${isSelected ? "selected" : ""}">
							<div class="msg-head">
								<div class="msg-author-wrap">
									<span class="pill ${m.direction}">${m.direction}</span>
									<span class="msg-author">${escapeHtml(m.author)}</span>
									${m.relation ? `<span class="pill">${escapeHtml(m.relation)}</span>` : ""}
									${m.work ? `<span class="pill">work:${escapeHtml(m.work.unit)}</span>` : ""}
								</div>
								<div class="msg-meta">
									<span>#${escapeHtml(m.seq)}</span>
									<span>${escapeHtml(formatClock(m.at))}</span>
								</div>
							</div>
							<div class="msg-body">${escapeHtml(m.text)}</div>
							<div class="msg-foot">
								<button class="copy-chip" data-act="reply-msg" data-sender="${escapeHtml(m.ref.sender)}" data-id="${escapeHtml(m.ref.id)}" data-seq="${escapeHtml(m.seq)}" data-author="${escapeHtml(m.author)}" data-thread="${escapeHtml(tk)}">Reply</button>
								${caps.supportsSendMessage ? `<button class="copy-chip" data-act="ask-agent-msg" data-thread="${escapeHtml(tk)}" data-seq="${escapeHtml(m.seq)}">${escapeHtml(tr(lang, "askAgent"))}</button>` : ""}
							</div>
						</div>
					`;
				}).join("");
		} else {
			mainHeadTitle = "Onlyne";
			mainHeadSub = snap.status.node_id;
			mainBodyHtml = `<div class="empty-state">${escapeHtml(tr(lang, "selectPrompt"))}</div>`;
		}

		if (state.mainView === "thread" || state.mainView === "inbox") {
			composerHtml = `
				<div class="composer">
					<div class="composer-bar">
						<label>${escapeHtml(tr(lang, "target"))}:
							<select id="comp-target">
								<option value="">${escapeHtml(tr(lang, "auto"))}</option>
								${allPeerIds.map(id => `<option value="${escapeHtml(id)}" ${state.composerTarget === id ? "selected" : ""}>${escapeHtml(shortId(id))}</option>`).join("")}
							</select>
						</label>
						<label>${escapeHtml(tr(lang, "kind"))}:
							<select id="comp-kind">
								<option value="note" ${state.composerKind === "note" ? "selected" : ""}>note</option>
								<option value="question" ${state.composerKind === "question" ? "selected" : ""}>question</option>
								<option value="answer" ${state.composerKind === "answer" ? "selected" : ""}>answer</option>
								<option value="handoff" ${state.composerKind === "handoff" ? "selected" : ""}>handoff</option>
								<option value="notify" ${state.composerKind === "notify" ? "selected" : ""}>notify</option>
							</select>
						</label>
						<label>${escapeHtml(tr(lang, "plan"))}:
							<select id="comp-resp">
								<option value="async" ${state.composerResponse === "async" ? "selected" : ""}>async</option>
								<option value="await" ${state.composerResponse === "await" ? "selected" : ""}>await</option>
								<option value="none" ${state.composerResponse === "none" ? "selected" : ""}>none</option>
							</select>
						</label>
						${state.replyTarget ? `
							<span class="pill">${escapeHtml(tr(lang, "replyingTo"))} #${escapeHtml(state.replyTarget.seq)} (${escapeHtml(state.replyTarget.author)})</span>
							<button class="copy-chip" data-act="clear-reply">${escapeHtml(tr(lang, "clear"))}</button>
						` : ""}
					</div>
					<div class="composer-row">
						<textarea id="comp-text" placeholder="Message...">${escapeHtml(state.composerText)}</textarea>
						<button class="btn primary" ${state.busy ? "disabled" : ""} data-act="send-msg">${escapeHtml(tr(lang, "send"))}</button>
					</div>
				</div>
			`;
		}

		const peerDetailHtml = (() => {
			if (!selectedPeer || !state.selectedPeerId) return "";
			const id = state.selectedPeerId;
			const trEntry = selectedPeer.trust;
			const disc = selectedPeer.discovery;
			const phase = trEntry?.phase ?? "discovered";
			const fp = trEntry?.fingerprint ?? "";
			const url = trEntry?.url ?? disc?.base_url ?? "";
			const seen = trEntry?.last_seen ?? disc?.last_seen ?? "";
			return `
				<div class="detail">
					<dl class="kv">
						<dt>${escapeHtml(tr(lang, "nodeLabel"))}</dt><dd>${escapeHtml(id)}</dd>
						<dt>${escapeHtml(tr(lang, "phaseLabel"))}</dt><dd>${escapeHtml(phase)}</dd>
						${fp ? `<dt>${escapeHtml(tr(lang, "fpLabel"))}</dt><dd>${escapeHtml(fp)}</dd>` : ""}
						${url ? `<dt>${escapeHtml(tr(lang, "urlLabel"))}</dt><dd>${escapeHtml(url)}</dd>` : ""}
						${seen ? `<dt>${escapeHtml(tr(lang, "seenLabel"))}</dt><dd>${escapeHtml(formatClock(seen))}</dd>` : ""}
						${disc?.source ? `<dt>${escapeHtml(tr(lang, "sourceLabel"))}</dt><dd>${escapeHtml(disc.source)}</dd>` : ""}
					</dl>
					<div class="detail-actions">
						${(!trEntry || phase === "revoked") ? `<button class="btn primary" ${state.busy ? "disabled" : ""} data-act="trust-begin" data-node="${escapeHtml(id)}">${escapeHtml(tr(lang, "beginExchange"))}</button>` : ""}
						${phase === "invited" ? `
							<button class="btn primary" ${state.busy ? "disabled" : ""} data-act="trust-accept" data-node="${escapeHtml(id)}">${escapeHtml(tr(lang, "accept"))}</button>
							<button class="btn danger" ${state.busy ? "disabled" : ""} data-act="trust-revoke" data-node="${escapeHtml(id)}">${escapeHtml(tr(lang, "rejectTrust"))}</button>
						` : ""}
						${(phase === "exchanged" || phase === "requested") ? `
							<button class="btn" data-act="compose-to" data-node="${escapeHtml(id)}">${escapeHtml(tr(lang, "composeToPeer"))}</button>
							<button class="btn danger" ${state.busy ? "disabled" : ""} data-act="trust-revoke" data-node="${escapeHtml(id)}">${escapeHtml(tr(lang, "revoke"))}</button>
						` : ""}
					</div>
				</div>
			`;
		})();

		const membersHtml = `
			<aside class="members">
				<div class="seed-box">
					<div class="seed-row">
						<input id="seed-url" placeholder="${escapeHtml(tr(lang, "connectSeed"))}" value="${escapeHtml(state.seedInput)}" />
						<button class="btn primary" ${state.busy ? "disabled" : ""} data-act="connect-seed">${escapeHtml(tr(lang, "connect"))}</button>
					</div>
					<div class="detail-actions">
						<button class="btn" ${state.busy ? "disabled" : ""} data-act="refresh">${escapeHtml(tr(lang, "refresh"))}</button>
					</div>
				</div>
				<div class="members-scroll">
					<div class="section-label">Peers (${allPeerIds.length})</div>
					${allPeerIds.map(id => {
						const info = findPeerInfo(snap, id);
						const phase = info.trust?.phase;
						const dotCls = phase === "exchanged" ? "ok" : phase === "invited" || phase === "requested" ? "warn" : phase === "revoked" ? "bad" : "";
						return `
							<button class="row-item ${state.selectedPeerId === id ? "active" : ""}" data-act="select-peer" data-peer="${escapeHtml(id)}">
								<span class="dot ${dotCls}"></span>
								<span class="name">${escapeHtml(shortId(id))}</span>
								<span class="muted" style="font-size:10.5px;">${escapeHtml(phase ?? "new")}</span>
							</button>
						`;
					}).join("")}
					<div class="section-label">Contexts (${snap.contexts.length})</div>
					${snap.contexts.map(c => `
						<div class="row-item">
							<span class="dot ${c.state === "running" || c.state === "idle" ? "ok" : "warn"}"></span>
							<span class="name">${escapeHtml(c.alias)}</span>
							<span class="muted" style="font-size:10.5px;">${escapeHtml(c.state)}</span>
						</div>
					`).join("")}
				</div>
				${peerDetailHtml}
			</aside>
		`;

		const mobileNavHtml = `
			<nav class="mobile-nav">
				<button class="${state.mobilePane === "channels" ? "active" : ""}" data-act="mobile-pane" data-pane="channels">${escapeHtml(tr(lang, "channels"))} (${snap.threads.length})</button>
				<button class="${state.mobilePane === "main" ? "active" : ""}" data-act="mobile-pane" data-pane="main">${escapeHtml(tr(lang, "viewMain"))}</button>
				<button class="${state.mobilePane === "members" ? "active" : ""}" data-act="mobile-pane" data-pane="members">${escapeHtml(tr(lang, "peers"))} (${allPeerIds.length})</button>
				<button data-act="toggle-lang">${lang === "zh-CN" ? "EN" : "中文"}</button>
				<button data-act="refresh">↻</button>
			</nav>
		`;

		root.innerHTML = `
			<div class="app" data-pane="${state.mobilePane}">
				${mobileNavHtml}
				${railHtml}
				${sidebarHtml}
				<main class="main">
					<header class="main-head">
						<div class="main-head-title">
							<h1>${escapeHtml(mainHeadTitle)}</h1>
							<div class="sub">${escapeHtml(mainHeadSub)}</div>
						</div>
						<div class="head-actions">
							${caps.supportsModelContext && selectedThread && state.mainView === "thread" ? `
								<button class="btn" data-act="pin-context">${escapeHtml(tr(lang, "pinContext"))}</button>
							` : ""}
						</div>
					</header>
					${requestBarHtml}
					${bannerHtml}
					<div class="main-scroll">
						${mainBodyHtml}
					</div>
					${composerHtml}
				</main>
				${membersHtml}
			</div>
		`;
	};

	root.addEventListener("input", ev => {
		const el = ev.target as HTMLElement | null;
		if (!el) return;
		if (el.id === "seed-url") state.seedInput = (el as HTMLInputElement).value;
		else if (el.id === "comp-text") state.composerText = (el as HTMLTextAreaElement).value;
		else if (el.id === "comp-target") state.composerTarget = (el as HTMLSelectElement).value;
		else if (el.id === "comp-kind") state.composerKind = (el as HTMLSelectElement).value;
		else if (el.id === "comp-resp") state.composerResponse = (el as HTMLSelectElement).value as "async" | "await" | "none";
		else if (el.id === "new-thread-title") state.newThreadTitle = (el as HTMLInputElement).value;
		else if (el.id === "policy-select") state.policyDraft = (el as HTMLSelectElement).value as AcquirePolicy;
		else if (el.id === "policy-allow") state.allowDraft = (el as HTMLTextAreaElement).value;
	});

	root.addEventListener("change", ev => {
		const el = ev.target as HTMLElement | null;
		if (!el) return;
		const act = el.getAttribute("data-act");
		if (act === "todo-status") {
			const id = el.getAttribute("data-id");
			const status = (el as HTMLSelectElement).value;
			if (id) void runAction("todo", { id, status });
		}
	});

	root.addEventListener("click", ev => {
		const target = (ev.target as HTMLElement | null)?.closest("[data-act]") as HTMLElement | null;
		if (!target) return;
		const act = target.getAttribute("data-act");
		if (!act || !state.snapshot) return;

		if (act === "mobile-pane") {
			state.mobilePane = (target.getAttribute("data-pane") as MobilePane) ?? "main";
			render();
		} else if (act === "select-domain") {
			state.selectedDomain = target.getAttribute("data-domain") ?? "all";
			if (state.mainView === "setup") state.mainView = "thread";
			render();
		} else if (act === "open-view") {
			state.mainView = (target.getAttribute("data-view") as MainView) ?? "thread";
			state.mobilePane = "main";
			render();
		} else if (act === "select-thread") {
			state.selectedThreadKey = target.getAttribute("data-thread") ?? undefined;
			state.mainView = "thread";
			state.mobilePane = "main";
			render();
		} else if (act === "select-peer") {
			state.selectedPeerId = target.getAttribute("data-peer") ?? undefined;
			state.mobilePane = "members";
			render();
		} else if (act === "toggle-new-thread") {
			state.showNewThread = !state.showNewThread;
			render();
		} else if (act === "create-thread") {
			const title = (root.querySelector("#new-thread-title") as HTMLInputElement | null)?.value.trim() ?? "";
			if (title) {
				state.newThreadTitle = "";
				state.showNewThread = false;
				void runAction("thread/create", { title }, `Created thread ${title}`);
			}
		} else if (act === "connect-seed") {
			const url = (root.querySelector("#seed-url") as HTMLInputElement | null)?.value.trim() ?? "";
			if (url) {
				state.seedInput = "";
				void runAction("connect", { url }, `Connected ${url}`);
			}
		} else if (act === "refresh") {
			void runAction("refresh");
		} else if (act === "toggle-lang") {
			const nextLang = state.snapshot.language === "zh-CN" ? "en" : "zh-CN";
			void runAction("language", { language: nextLang });
		} else if (act === "copy") {
			const val = target.getAttribute("data-copy") ?? "";
			void navigator.clipboard?.writeText(val);
			showBanner("info", `Copied ${val}`);
		} else if (act === "clear-banner") {
			state.banner = undefined;
			render();
		} else if (act === "clear-reply") {
			state.replyTarget = undefined;
			render();
		} else if (act === "clear-exchange") {
			state.pendingExchange = undefined;
			render();
		} else if (act === "reply-msg") {
			state.replyTarget = {
				sender: target.getAttribute("data-sender") ?? "",
				id: target.getAttribute("data-id") ?? "",
				seq: target.getAttribute("data-seq") ?? "",
				author: target.getAttribute("data-author") ?? "",
				threadKey: target.getAttribute("data-thread") ?? "",
			};
			state.composerKind = "answer";
			state.mobilePane = "main";
			render();
		} else if (act === "compose-to") {
			state.composerTarget = target.getAttribute("data-node") ?? "";
			state.mainView = "thread";
			state.mobilePane = "main";
			render();
		} else if (act === "send-msg") {
			const body = state.composerText.trim();
			if (!body) return;
			const thread = state.snapshot.threads.find(t => threadKey(t) === state.selectedThreadKey);
			const payload: Record<string, unknown> = {
				delivery_kind: state.composerKind,
				msg: body,
				response: state.composerResponse,
				execution: "existing",
			};
			if (state.replyTarget) {
				payload.in_reply_to = { sender: state.replyTarget.sender, id: state.replyTarget.id };
				payload.relation = "reply";
			} else if (thread) {
				payload.thread = thread.ref;
			}
			if (state.composerTarget) payload.to = state.composerTarget;
			state.composerText = "";
			state.replyTarget = undefined;
			void runAction("send", payload, "Message dispatched");
		} else if (act === "trust-begin") {
			const nodeId = target.getAttribute("data-node");
			if (!nodeId) return;
			void runAction("trust/begin", { node_id: nodeId }).then(res => {
				if (res && typeof res === "object" && "fingerprint" in res) {
					state.pendingExchange = res as UiState["pendingExchange"];
					state.mobilePane = "main";
					render();
				}
			});
		} else if (act === "trust-confirm") {
			const nodeId = target.getAttribute("data-node");
			const fp = target.getAttribute("data-fp");
			if (!nodeId || !fp) return;
			state.pendingExchange = undefined;
			void runAction("trust/confirm", { node_id: nodeId, fingerprint: fp }, `Confirmed fingerprint for ${shortId(nodeId)}`);
		} else if (act === "trust-accept") {
			const nodeId = target.getAttribute("data-node");
			if (nodeId) void runAction("trust/accept", { node_id: nodeId }, `Approved ${shortId(nodeId)}`);
		} else if (act === "trust-revoke") {
			const nodeId = target.getAttribute("data-node");
			if (nodeId) void runAction("trust/revoke", { node_id: nodeId }, `Revoked ${shortId(nodeId)}`);
		} else if (act === "trust-rotate") {
			void runAction("trust/rotate", {}, "Rotated local identity key");
		} else if (act === "save-policy") {
			const policy = state.policyDraft ?? state.snapshot.acquisition.policy;
			const rawAllow = state.allowDraft ?? state.snapshot.acquisition.allow.join("\n");
			const allow = rawAllow.split(/[,\n]/).map(s => s.trim()).filter(Boolean);
			void runAction("acquisition", { policy, allow }, "Saved acquisition policy");
		} else if (act === "save-config") {
			const val = (id: string): string => (root.querySelector(id) as HTMLInputElement | HTMLSelectElement | null)?.value.trim() ?? "";
			const portNum = Number(val("#cfg-port"));
			const pageNum = Number(val("#cfg-page")) || 50;
			const seeds = val("#cfg-seeds").split(/[,\n]/).map(s => s.trim()).filter(Boolean);
			const allow = val("#cfg-allow").split(/[,\n]/).map(s => s.trim()).filter(Boolean);
			void runAction("config", {
				region: val("#cfg-region") || "local",
				node_id: val("#cfg-node") || "agy-node",
				bind: val("#cfg-bind") || "127.0.0.1",
				...(Number.isInteger(portNum) && portNum > 0 ? { port: portNum } : {}),
				key_scope: val("#cfg-scope") || "project",
				page_size: pageNum,
				seeds,
				policy: val("#cfg-policy") || "segment",
				allow,
				judge_mode: val("#cfg-judge-mode") || "auto",
				...(val("#cfg-judge-model") ? { judge_model: val("#cfg-judge-model") } : {}),
				...(val("#cfg-judge-base") ? { judge_base_url: val("#cfg-judge-base") } : {}),
			}, "Configuration saved & node started");
			state.setupDraft = undefined;
		} else if (act === "pin-context") {
			const thread = state.snapshot.threads.find(t => threadKey(t) === state.selectedThreadKey);
			if (!thread) return;
			const lines = thread.messages.slice(-8).map(m => `[#${m.seq} ${m.author}] ${m.text}`);
			void transport.hostActions().updateAgentContext(`Onlyne thread ${threadKey(thread)} (${thread.title}):\n${lines.join("\n")}`)
				.then(() => showBanner("info", "Pinned thread excerpt to Antigravity context"));
		} else if (act === "ask-agent-msg") {
			const tk = target.getAttribute("data-thread") ?? "";
			const seq = target.getAttribute("data-seq") ?? "";
			void transport.hostActions().sendAgentMessage(
				`Please review Onlyne message onlyne://threads/${encodeURIComponent(tk.split("/")[0] ?? "")}/${encodeURIComponent(tk.split("/")[1] ?? "")}/messages/${seq} and draft/send a response if appropriate.`,
			);
		}
	});

	root.addEventListener("focusout", () => {
		window.setTimeout(() => {
			if (deferredRender && !isEditingInput()) {
				deferredRender = false;
				render();
			}
		}, 120);
	});

	transport = await createGuiTransport();
	await refresh();
	transport.subscribe(() => {
		void refresh(true).catch(() => undefined);
	});
}

if (typeof document !== "undefined") {
	const root = document.getElementById("app");
	if (root) {
		void mountOnlyneGui(root);
	}
}
