import type { TrustPhase } from "@onlyne/lib";
import { pickText, type Locale } from "../i18n.ts";

export interface TuiStrings {
	appTitle: string;
	unconfiguredBanner: string;
	loading: string;
	columns: {
		rail: string;
		sidebar: string;
		flow: string;
		members: string;
	};
	rail: {
		allDomains: string;
		joinDomain: string;
		empty: string;
	};
	sidebar: {
		pendingSection: string;
		threadsSection: string;
		newThread: string;
		inbox: string;
		trust: string;
		todos: string;
		emptyThreads: string;
		closedTag: string;
		readOnlyTag: string;
	};
	flow: {
		empty: string;
		approvalsTitle: string;
		peerFp: string;
		localFp: string;
		pressAcceptReject: string;
		threadEmpty: string;
		messageDetailHint: string;
		inboxTitle: string;
		inboxEmpty: string;
		deliveriesTitle: string;
		deliveriesEmpty: string;
		trustTitle: string;
		trustPolicy: string;
		trustAllow: string;
		trustRotateKey: string;
		trustEditRules: string;
		trustAuditTitle: string;
		trustAuditEmpty: string;
		todosTitle: string;
		todosEmpty: string;
		todoCycleHint: string;
	};
	members: {
		connectBtn: string;
		refreshBtn: string;
		trustedSection: string;
		discoveredSection: string;
		empty: string;
		detailTitle: string;
		detailRegion: string;
		detailUrl: string;
		detailSeen: string;
		detailPhase: string;
		detailFp: string;
		detailActions: string;
	};
	trustPhase: Record<TrustPhase, string>;
	footer: {
		hintsWide: string;
		hintsCompact: string;
		langLabel: string;
		initLabel: string;
		helpLabel: string;
		quitLabel: string;
	};
	modals: {
		helpTitle: string;
		helpLines: string[];
		connectTitle: string;
		connectPrompt: string;
		composeTitle: string;
		composeReplyTo: string;
		composeTo: string;
		composeThread: string;
		composeMode: string;
		composeHint: string;
		newThreadTitle: string;
		newThreadPrompt: string;
		joinDomainTitle: string;
		joinDomainPrompt: string;
		exchangeTitle: string;
		exchangeWarn: string;
		exchangeAction: string;
		confirmAcceptTitle: string;
		confirmRevokeTitle: string;
		confirmPrompt: string;
		acqTitle: string;
		acqPolicy: string;
		acqAllow: string;
		acqHint: string;
		msgDetailTitle: string;
		msgDetailActions: string;
	};
	bootstrap: {
		title: string;
		subtitle: string;
		save: string;
		cancel: string;
		hint: string;
		fields: {
			region: { label: string; hint: string };
			node_id: { label: string; hint: string };
			bind: { label: string; hint: string };
			key_scope: { label: string; hint: string };
			page_size: { label: string; hint: string };
			seeds: { label: string; hint: string };
			policy: { label: string; hint: string };
			allow: { label: string; hint: string };
			judge_mode: { label: string; hint: string };
			judge_model: { label: string; hint: string };
			judge_base_url: { label: string; hint: string };
		};
		errors: {
			regionRequired: string;
			nodeIdRequired: string;
			bindRequired: string;
			keyScopeInvalid: string;
			pageSizeInvalid: string;
			policyInvalid: string;
			allowInvalid: string;
			judgeModeInvalid: string;
		};
		saved: string;
	};
	notices: {
		reloaded: string;
		connected: string;
		sent: string;
		trusted: string;
		revoked: string;
		keyRotated: string;
		exchangeSent: string;
		acqUpdated: string;
		domainJoined: string;
		threadCreated: string;
		todoUpdated: string;
		readOnlyThread: string;
		noComposeTarget: string;
		noNodeSelected: string;
	};
}

const EN: TuiStrings = {
	appTitle: "Onlyne · Antigravity TUI",
	unconfiguredBanner: "Node not configured yet. Press [i] to open Setup Wizard.",
	loading: "Connecting to Onlyne daemon…",
	columns: {
		rail: "Domains",
		sidebar: "Workspace",
		flow: "Conversation",
		members: "Peers",
	},
	rail: {
		allDomains: "* All Domains",
		joinDomain: "+ Join Domain",
		empty: "(none)",
	},
	sidebar: {
		pendingSection: "Pending Keys",
		threadsSection: "Threads",
		newThread: "+ New Thread",
		inbox: "Inbox / Queue",
		trust: "Trust & Rules",
		todos: "Onlyne Todos",
		emptyThreads: "(no threads)",
		closedTag: "closed",
		readOnlyTag: "ro",
	},
	flow: {
		empty: "Select a thread on the left or pick a peer on the right ([c] compose, [i] setup).",
		approvalsTitle: "Key Exchange Requests Waiting For You",
		peerFp: "Peer FP ",
		localFp: "Local FP",
		pressAcceptReject: "Keys: [a] Accept  [x] Reject  [k] Inspect peer",
		threadEmpty: "No messages in this thread yet. Press [c] to write.",
		messageDetailHint: "Press [Enter] on message for full view · [c] compose · [y] reply latest",
		inboxTitle: "Inbox, Queue & Delivery Confirmations",
		inboxEmpty: "Queue is clean.",
		deliveriesTitle: "Pending Antigravity Wakes",
		deliveriesEmpty: "No pending unconfirmed wakes.",
		trustTitle: "Node Key & Trust Policy",
		trustPolicy: "Acquisition policy",
		trustAllow: "CIDR allow list",
		trustRotateKey: "[K] Rotate local key",
		trustEditRules: "[e] Edit acquisition rules",
		trustAuditTitle: "Trust & Acquisition Audit",
		trustAuditEmpty: "No audit records yet.",
		todosTitle: "Onlyne Work Items",
		todosEmpty: "No work items.",
		todoCycleHint: "Press [Space] or [Enter] on item to cycle status",
	},
	members: {
		connectBtn: "[+ Connect Seed]",
		refreshBtn: "[Refresh Discovery]",
		trustedSection: "Trust Ledger",
		discoveredSection: "Discovered Peers",
		empty: "(no peers yet)",
		detailTitle: "Peer Detail",
		detailRegion: "Region",
		detailUrl: "URL   ",
		detailSeen: "Seen  ",
		detailPhase: "Trust ",
		detailFp: "FP    ",
		detailActions: "Actions: [k] Exchange/Verify  [a] Accept  [x] Revoke  [c] Message",
	},
	trustPhase: {
		exchanged: "verified",
		requested: "waiting peer",
		invited: "waiting you",
		revoked: "revoked",
	},
	footer: {
		hintsWide: "Tab/1-4:pane  ↑↓/jk:move  Enter:open  c:compose  y:reply  k:keys  a:accept  x:reject  s:seed  r:refresh",
		hintsCompact: "1-4:pane ↑↓:move Enter:open c:msg k:keys r:refresh",
		langLabel: "L:lang",
		initLabel: "i:init",
		helpLabel: "?:help",
		quitLabel: "q:quit",
	},
	modals: {
		helpTitle: "Keyboard Shortcuts",
		helpLines: [
			"1-4 / Tab / h l : Switch active column (Domains / Sidebar / Flow / Peers)",
			"↑ ↓ / j k       : Move cursor / scroll flow",
			"Enter / Space   : Open item / activate button / read full message",
			"c               : Compose message (in active thread or to selected peer)",
			"y               : Reply to latest (or opened) message in active thread",
			"s               : Connect seed node by URL",
			"r               : Refresh discovery and snapshot",
			"k               : Start interactive key exchange with selected peer",
			"a / x           : Accept / Reject (or revoke) key exchange",
			"e               : Edit acquisition policy (all / segment / none)",
			"K               : Rotate local node key",
			"i               : Open node Setup Wizard (.onlyne/config.toml)",
			"L               : Toggle language (en / zh-CN)",
			"q / Ctrl+C      : Quit TUI (daemon stays alive if running)",
		],
		connectTitle: "Connect Seed Node",
		connectPrompt: "Enter origin (http://host:7801) or service URL:",
		composeTitle: "Compose Message (msg)",
		composeReplyTo: "Replying to",
		composeTo: "Recipient ",
		composeThread: "Thread    ",
		composeMode: "Response  ",
		composeHint: "Enter: newline · Ctrl+S: Send · Tab: cycle response mode · Esc: cancel",
		newThreadTitle: "Create Thread",
		newThreadPrompt: "Enter thread title:",
		joinDomainTitle: "Join Domain",
		joinDomainPrompt: "Enter domain URN or ID:",
		exchangeTitle: "Confirm Key Exchange Fingerprint",
		exchangeWarn: "Compare BOTH fingerprints out-of-band with the peer before confirming.",
		exchangeAction: "Enter: confirm & send request · Esc: cancel",
		confirmAcceptTitle: "Accept Incoming Key Exchange",
		confirmRevokeTitle: "Revoke Trust Key",
		confirmPrompt: "Press Enter to confirm or Esc to cancel.",
		acqTitle: "Edit Acquisition Rules",
		acqPolicy: "Policy (Space cycles all/segment/none)",
		acqAllow: "CIDR Allow List (comma separated)      ",
		acqHint: "Tab: switch field · Enter/Ctrl+S: save · Esc: cancel",
		msgDetailTitle: "Full Message",
		msgDetailActions: "Keys: [y] Reply to this  [c] Message author  [Esc] Close",
	},
	bootstrap: {
		title: "Onlyne Antigravity Setup (.onlyne/config.toml)",
		subtitle: "Configure node identity, HTTP bind, discovery seeds, acquisition & SystemOne judge.",
		save: "[ Save Configuration (Ctrl+S) ]",
		cancel: "[ Cancel (Esc) ]",
		hint: "↑↓/Tab: next field · ←→/Edit: value · Ctrl+S: save · Esc: cancel",
		fields: {
			region: { label: "region", hint: "Region name or URN (e.g. local, shanghai, urn:onlyne:region:local)" },
			node_id: { label: "node.id", hint: "Agent short name or full URN (e.g. agy-alpha)" },
			bind: { label: "server.bind", hint: "Listen IP (127.0.0.1 for local, 0.0.0.0 for LAN)" },
			key_scope: { label: "key_scope", hint: "project | global | none" },
			page_size: { label: "page_size", hint: "Messages per page (1..200)" },
			seeds: { label: "seeds", hint: "Comma-separated peer URLs (optional)" },
			policy: { label: "policy", hint: "Peer acquisition: all | segment | none" },
			allow: { label: "allow", hint: "CIDR list for segment policy (e.g. 127.0.0.1/32, 10.0.0.0/8)" },
			judge_mode: { label: "judge.mode", hint: "auto | sampling | openai | gemini | agy-cli | none" },
			judge_model: { label: "judge.model", hint: "Optional model name (blank = default)" },
			judge_base_url: { label: "judge.base_url", hint: "Optional OpenAI/Gemini base URL" },
		},
		errors: {
			regionRequired: "region is required",
			nodeIdRequired: "node.id is required",
			bindRequired: "server.bind is required",
			keyScopeInvalid: "key_scope must be project, global, or none",
			pageSizeInvalid: "page_size must be an integer between 1 and 200",
			policyInvalid: "policy must be all, segment, or none",
			allowInvalid: "invalid CIDR allow entry",
			judgeModeInvalid: "judge.mode must be auto, sampling, openai, gemini, agy-cli, or none",
		},
		saved: "Configuration saved and daemon reloaded.",
	},
	notices: {
		reloaded: "Snapshot refreshed.",
		connected: "Connected seed: ",
		sent: "Message dispatched.",
		trusted: "Trust exchange accepted.",
		revoked: "Trust entry revoked.",
		keyRotated: "Local node key rotated.",
		exchangeSent: "Key exchange request sent.",
		acqUpdated: "Acquisition policy saved.",
		domainJoined: "Joined domain.",
		threadCreated: "Thread created.",
		todoUpdated: "Todo updated.",
		readOnlyThread: "Thread is read-only here (hosted remotely); compose to peer instead.",
		noComposeTarget: "Select a thread or peer first.",
		noNodeSelected: "Select a peer first.",
	},
};

const ZH: TuiStrings = {
	appTitle: "Onlyne · Antigravity 终端面板",
	unconfiguredBanner: "节点尚未配置。按 [i] 打开初始化向导。",
	loading: "正在连接 Onlyne 守护进程…",
	columns: {
		rail: "域",
		sidebar: "工作区",
		flow: "对话流",
		members: "节点",
	},
	rail: {
		allDomains: "* 全部域",
		joinDomain: "+ 加入域",
		empty: "(空)",
	},
	sidebar: {
		pendingSection: "待确认密钥",
		threadsSection: "线程",
		newThread: "+ 新建线程",
		inbox: "收件箱 / 队列",
		trust: "信任与规则",
		todos: "Onlyne 待办",
		emptyThreads: "(暂无线程)",
		closedTag: "已关闭",
		readOnlyTag: "只读",
	},
	flow: {
		empty: "在左侧打开线程，或在右侧选择节点（[c] 发消息，[i] 初始化配置）。",
		approvalsTitle: "等待你确认的密钥交换请求",
		peerFp: "对端指纹",
		localFp: "本机指纹",
		pressAcceptReject: "快捷键: [a] 同意  [x] 拒绝  [k] 查看节点",
		threadEmpty: "当前线程尚无消息。按 [c] 撰写消息。",
		messageDetailHint: "在消息上按 [Enter] 查看全文 · [c] 发消息 · [y] 回复最新",
		inboxTitle: "收件箱、消息队列与唤醒确认",
		inboxEmpty: "队列为空。",
		deliveriesTitle: "待消费的 Antigravity 唤醒",
		deliveriesEmpty: "暂无未确认的投递。",
		trustTitle: "节点密钥与入网信任策略",
		trustPolicy: "自动发现策略",
		trustAllow: "CIDR 白名单",
		trustRotateKey: "[K] 轮换本机密钥",
		trustEditRules: "[e] 编辑入网规则",
		trustAuditTitle: "信任与发现审计日志",
		trustAuditEmpty: "暂无审计记录。",
		todosTitle: "Onlyne 协作待办",
		todosEmpty: "暂无待办。",
		todoCycleHint: "按 [Space] 或 [Enter] 切换任务状态",
	},
	members: {
		connectBtn: "[+ 连接节点]",
		refreshBtn: "[刷新发现]",
		trustedSection: "信任账本",
		discoveredSection: "已发现节点",
		empty: "(暂无节点)",
		detailTitle: "节点详情",
		detailRegion: "区域  ",
		detailUrl: "地址  ",
		detailSeen: "活跃  ",
		detailPhase: "信任  ",
		detailFp: "指纹  ",
		detailActions: "操作: [k] 交换密钥  [a] 同意  [x] 撤销  [c] 发消息",
	},
	trustPhase: {
		exchanged: "已验证",
		requested: "等待对端",
		invited: "等待你确认",
		revoked: "已撤销",
	},
	footer: {
		hintsWide: "Tab/1-4:切栏  ↑↓/jk:移动  Enter:打开  c:发信  y:回复  k:换钥  a:同意  x:拒绝  s:连节点  r:刷新",
		hintsCompact: "1-4:切栏 ↑↓:移动 Enter:打开 c:发信 k:换钥 r:刷新",
		langLabel: "L:语言",
		initLabel: "i:配置",
		helpLabel: "?:帮助",
		quitLabel: "q:退出",
	},
	modals: {
		helpTitle: "快捷键说明",
		helpLines: [
			"1-4 / Tab / h l : 切换面板栏（域 / 工作区 / 对话流 / 节点）",
			"↑ ↓ / j k       : 上下移动光标 / 滚动消息流",
			"Enter / Space   : 打开项目 / 触发按钮 / 阅读完整消息",
			"c               : 撰写消息（发往当前线程或选中节点）",
			"y               : 回复当前线程最新（或当前打开）的消息",
			"s               : 输入 URL 连接种子节点",
			"r               : 刷新网络发现与实时状态",
			"k               : 与选中节点发起交互式密钥交换（带指纹核对）",
			"a / x           : 同意 / 拒绝（或撤销）密钥",
			"e               : 编辑节点入网策略（all / segment / none）",
			"K               : 轮换本机节点密钥",
			"i               : 打开节点配置向导（.onlyne/config.toml）",
			"L               : 切换界面语言（en / zh-CN）",
			"q / Ctrl+C      : 退出终端面板（后台守护进程继续运行）",
		],
		connectTitle: "连接种子节点",
		connectPrompt: "输入节点源地址（http://host:7801）或完整服务 URL：",
		composeTitle: "撰写消息（msg）",
		composeReplyTo: "回复对象",
		composeTo: "接收节点",
		composeThread: "所属线程",
		composeMode: "响应模式",
		composeHint: "Enter: 换行 · Ctrl+S: 发送 · Tab: 切换响应模式 · Esc: 取消",
		newThreadTitle: "新建线程",
		newThreadPrompt: "输入线程标题：",
		joinDomainTitle: "加入域",
		joinDomainPrompt: "输入域 URN 或 ID：",
		exchangeTitle: "核对并确认密钥指纹",
		exchangeWarn: "请在带外与对方同时核对双方指纹，确认一致后再发送请求。",
		exchangeAction: "Enter: 确认并发送密钥交换请求 · Esc: 取消",
		confirmAcceptTitle: "确认接受密钥交换",
		confirmRevokeTitle: "撤销节点信任",
		confirmPrompt: "按 Enter 确认，或按 Esc 取消。",
		acqTitle: "编辑节点入网规则",
		acqPolicy: "策略（空格切换 all / segment / none）",
		acqAllow: "CIDR 允许列表（逗号分隔）             ",
		acqHint: "Tab: 切换输入框 · Enter/Ctrl+S: 保存 · Esc: 取消",
		msgDetailTitle: "完整消息",
		msgDetailActions: "按键: [y] 回复此消息  [c] 给作者发消息  [Esc] 关闭",
	},
	bootstrap: {
		title: "Onlyne Antigravity 初始化向导（.onlyne/config.toml）",
		subtitle: "配置节点标识、监听地址、种子节点、入网策略与 SystemOne 裁判模型。",
		save: "[ 保存配置 (Ctrl+S) ]",
		cancel: "[ 取消 (Esc) ]",
		hint: "↑↓/Tab: 切换字段 · 直接输入修改 · Ctrl+S: 保存 · Esc: 取消",
		fields: {
			region: { label: "region", hint: "区域短名或 URN（如 local、shanghai、urn:onlyne:region:local）" },
			node_id: { label: "node.id", hint: "节点短名或完整 URN（如 agy-alpha）" },
			bind: { label: "server.bind", hint: "监听地址（本机 127.0.0.1，局域网 0.0.0.0）" },
			key_scope: { label: "key_scope", hint: "project | global | none" },
			page_size: { label: "page_size", hint: "每页消息数量（1..200）" },
			seeds: { label: "seeds", hint: "逗号分隔的对端服务 URL（可选）" },
			policy: { label: "policy", hint: "节点入网策略：all | segment | none" },
			allow: { label: "allow", hint: "segment 策略使用的 CIDR 列表（如 127.0.0.1/32）" },
			judge_mode: { label: "judge.mode", hint: "auto | sampling | openai | gemini | agy-cli | none" },
			judge_model: { label: "judge.model", hint: "可选指定裁判模型名称（留空使用默认）" },
			judge_base_url: { label: "judge.base_url", hint: "可选 OpenAI/Gemini API Base URL" },
		},
		errors: {
			regionRequired: "region 不能为空",
			nodeIdRequired: "node.id 不能为空",
			bindRequired: "server.bind 不能为空",
			keyScopeInvalid: "key_scope 必须为 project、global 或 none",
			pageSizeInvalid: "page_size 必须为 1 到 200 的整数",
			policyInvalid: "policy 必须为 all、segment 或 none",
			allowInvalid: "包含无效的 CIDR 网段",
			judgeModeInvalid: "judge.mode 必须为 auto、sampling、openai、gemini、agy-cli 或 none",
		},
		saved: "配置已保存，守护进程已热重载。",
	},
	notices: {
		reloaded: "状态已刷新。",
		connected: "已连接种子节点：",
		sent: "消息已提交发送。",
		trusted: "已同意密钥交换。",
		revoked: "已撤销节点信任。",
		keyRotated: "本机节点密钥已轮换。",
		exchangeSent: "已发送密钥交换请求。",
		acqUpdated: "入网策略已保存。",
		domainJoined: "已加入域。",
		threadCreated: "线程已创建。",
		todoUpdated: "待办状态已更新。",
		readOnlyThread: "此线程由远端托管且在本节点只读；请直接向节点发消息。",
		noComposeTarget: "请先选择线程或对端节点。",
		noNodeSelected: "请先在右侧选择一个节点。",
	},
};

export function tuiText(locale: Locale): TuiStrings {
	return pickText("en", "zh-CN", locale) === "zh-CN" ? ZH : EN;
}
