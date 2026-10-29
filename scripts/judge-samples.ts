/**
 * Real-judge sample runs for the omp-onlyne JudgeDecider.
 *
 *   bun scripts/judge-samples.ts [--only=<surface-id>] [--out=<path>]
 *
 * The SystemOne gateway (`group/decider`) is wired into JudgeDecider through the plugin's
 * `deps.judge` seam: decider.ts calls it after `fitState` and before the OMP judge would be
 * resolved, so every question the plugin builds — real candidate rows, real instruction and
 * criteria texts — reaches the gateway verbatim and its answers come back unchanged. Everything
 * except the transport is the production decision path.
 *
 * Fixtures are synthetic but lifelike: titled threads, mixed-language bodies, three peers across
 * trust and discovery, registered subagent contexts. Outcomes are recorded, never fixed — a
 * mismatch is data for judge tuning, so the script exits 0 even with mismatches; only an
 * unreachable endpoint fails the run.
 *
 * Surfaces (36 scenarios):
 *   o1               O-1 intent, one input per intent
 *   o2a              O-2A original: which inbound message this call answers, if any
 *   relation         O-2A relation: reply vs followup on a referenced message
 *   target           O-2A target: which peer receives, named or unnamed
 *   response         O-3A response: none / async / await
 *   execution        O-3A execution: existing vs new, both isReply variants
 *   receiver-context O-3A-Ctx: which published context of the target continues
 *   inbound          I-0 needs_answer, I-1 routing, I-2 insertion via decideInbound
 */
import { mkdirSync } from "node:fs";
import { writeFile } from "node:fs/promises";
import { dirname } from "node:path";
import {
	JudgeDecider,
	type Answers,
	type DeciderDeps,
	type DecisionScope,
	type InboundDecision,
	type PeerRecord,
	type Questions,
	type ReceiverContext,
} from "../packages/omp-onlyne/src/decider.ts";
import type { ContextRecord, OnlyneConfig } from "../packages/omp-onlyne/src/types.ts";
import type { Action, DeliveryPlan, Entry, MsgInput, NodeId, Page, Thread, ThreadRef } from "../packages/lib/src/types.ts";

const ENDPOINT = process.env.ONLYNE_JUDGE_ENDPOINT ?? "http://127.0.0.1:3425/v1/systemone";
const MODEL = process.env.ONLYNE_JUDGE_MODEL ?? "group/decider";
const DEFAULT_OUT = "/tmp/judge-samples/omp-results.json";
const JUDGE_TIMEOUT_MS = 30_000;
const JUDGE_ATTEMPTS = 2; // first try plus one retry; then the scenario records the error

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const NOW = "2026-10-28T08:00:00Z";
const SELF: NodeId = "urn:onlyne:agent:main";
const CAERULEA: NodeId = "urn:onlyne:agent:caerulea";
const MIDNIGHT: NodeId = "urn:onlyne:agent:midnight";
const JUNCO: NodeId = "urn:onlyne:agent:junco";

function makeThread(id: string, title: string): Thread {
	return {
		thread_ref: { owner: SELF, id },
		title,
		state: "open",
		next_seq: "4",
		event_seq: "4",
		created_at: "2026-10-01T00:00:00Z",
		closed_at: null,
	};
}

function makeEntry(threadId: string, seq: string, sender: NodeId, kind: string, msg: string, id: string): Entry {
	return {
		thread_ref: { owner: SELF, id: threadId },
		seq,
		message: { id, sender, delivery_kind: kind, msg },
		authenticated_author: sender,
		reply_authority: sender,
		at: NOW,
	};
}

const T_RELEASE = makeThread("t-release", "Release coordination");
const T_DATA = makeThread("t-data", "数据迁移");
const T_RESEARCH = makeThread("t-research", "Research");

const E_RELEASE_ASK = makeEntry("t-release", "1", CAERULEA, "确认一下发布时间", "想确认一下 v3.0 的发布时间,你们定在周五了吗?麻烦回我。", "m-release-1");
const E_DATASET_ASK = makeEntry("t-data", "1", MIDNIGHT, "问一个数", "数据集 v2 的校验和是多少?迁移脚本要用,等这个数。", "m-dataset-1");
const E_GREET_ASK = makeEntry("t-research", "1", JUNCO, "问候并约评审", "你好,在吗?请问周五的评审你能参加吗?", "m-greet-1");
const E_PARAMS_ASK = makeEntry("t-research", "2", CAERULEA, "请确认参数", "请确认第二组参数是否按 42 执行,确认后我再跑回归。", "m-params-1");
const E_PROGRESS_NOTE = makeEntry("t-data", "2", MIDNIGHT, "进度汇报", "进度汇报:数据迁移第一批分片已完成 80%,暂无阻塞。", "m-progress-1");
const E_STATUS_SYNC = makeEntry("t-research", "3", JUNCO, "状态同步", "状态同步:文档站 v2 已部署完成,一切正常,无需其他操作。", "m-status-1");

const DEFAULT_THREADS: Thread[] = [T_RELEASE, T_DATA, T_RESEARCH];
const DEFAULT_MEMBERS: Record<string, NodeId[]> = {
	"t-release": [SELF, CAERULEA],
	"t-data": [SELF, MIDNIGHT],
	"t-research": [SELF, JUNCO, CAERULEA], // group thread: never reused for a private send
};
const DEFAULT_MESSAGES: Record<string, Entry[]> = {
	"t-release": [E_RELEASE_ASK],
	"t-data": [E_DATASET_ASK, E_PROGRESS_NOTE],
	"t-research": [E_GREET_ASK, E_PARAMS_ASK, E_STATUS_SYNC],
};
const DEFAULT_PEERS: PeerRecord[] = [
	{ node_id: CAERULEA, base_url: "http://127.0.0.1:7802", instance_id: "cae-1", last_seen: NOW },
	{ node_id: JUNCO, base_url: "http://127.0.0.1:7804", instance_id: "jun-1", last_seen: NOW },
];
const DEFAULT_TRUST: Array<{ node_id: NodeId; phase: string; base_url?: string }> = [
	{ node_id: MIDNIGHT, phase: "exchanged", base_url: "http://127.0.0.1:7803/onlyne/v3" },
];

const RC_MAIN: ReceiverContext = { alias: "main", summary: "主会话:节点事务与日常协调", state: "idle" };
const RC_MIGRATION: ReceiverContext = { alias: "migration", summary: "数据迁移任务上下文:分片校验与批次推进", state: "running" };
const RC_DOCS: ReceiverContext = { alias: "docs", summary: "文档站点维护上下文", state: "idle" };

function contextRecord(overrides: Partial<ContextRecord> & { alias: string }): ContextRecord {
	return {
		node_id: SELF,
		main_agent_id: "Main",
		summary: "",
		state: "idle",
		created_at: "2026-10-10T00:00:00Z",
		...overrides,
	};
}

const CTX_MIGRATION = contextRecord({
	alias: "ctx-1",
	agent_id: "Sub-1",
	summary: "数据迁移子代理:跑分片校验与批次推进",
	state: "running",
	thread_ref: { owner: SELF, id: "t-data" },
	bound_at: NOW,
});
const CTX_DOCS = contextRecord({
	alias: "ctx-2",
	agent_id: "Sub-2",
	summary: "文档站点子代理:部署与巡检",
	state: "idle",
	thread_ref: { owner: SELF, id: "t-research" },
	bound_at: NOW,
});
const CTX_LOST = contextRecord({
	alias: "c-9",
	agent_id: "Sub-1",
	summary: "已结束的早期迁移子代理",
	state: "lost",
	bound_at: "2026-10-20T09:00:00Z",
});

const DEFAULT_BRANCH = [
	{
		id: "u-1",
		type: "message",
		timestamp: "2026-10-28T07:58:00Z",
		message: {
			role: "user",
			content: [{ type: "text", text: "盯着发布流程和数据迁移,及时处理各节点的消息;需要答复的尽快答复,拿不准的先问我。" }],
		},
	},
];

interface InboxRow {
	context?: string;
	attention: { entry?: Entry; disposition?: string };
}

interface Fixture {
	threads?: Thread[];
	members?: Record<string, NodeId[]>;
	messagesByThread?: Record<string, Entry[]>;
	peers?: PeerRecord[];
	trust?: Array<{ node_id: NodeId; phase: string; base_url?: string }>;
	inbox?: InboxRow[];
	contexts?: ContextRecord[];
	receiverContexts?: Record<NodeId, ReceiverContext[]>;
	branch?: unknown[];
	/** Whether the OMP main session is idle; inbound insertion reads it as `is_turn_active`. */
	idle?: boolean;
}

// ---------------------------------------------------------------------------
// SystemOne client (self-contained; mirrors the judge() contract, no tern imports)
// ---------------------------------------------------------------------------

interface JudgeCall {
	keys: string[];
	questions: Questions;
	state: Record<string, unknown>;
	answers: Answers;
	ms: number;
	error?: string;
}

async function postToSystemOne(state: unknown, questions: Questions): Promise<Answers> {
	const response = await fetch(ENDPOINT, {
		method: "POST",
		headers: { "content-type": "application/json" },
		body: JSON.stringify({ model: MODEL, state, questions }),
		signal: AbortSignal.timeout(JUDGE_TIMEOUT_MS),
	});
	const body = await response.text();
	if (!response.ok) throw new Error(`SystemOne answered HTTP ${String(response.status)}: ${body.slice(0, 300)}`);
	let parsed: unknown;
	try {
		parsed = JSON.parse(body);
	} catch {
		throw new Error("SystemOne returned no JSON object");
	}
	const row = parsed as { answers?: unknown } | null;
	if (row === null || typeof row !== "object" || row.answers === null || typeof row.answers !== "object" || Array.isArray(row.answers)) {
		throw new Error(`SystemOne response carried no answers object: ${body.slice(0, 200)}`);
	}
	return row.answers as Answers;
}

/** The `deps.judge` seam: forwards the plugin's real questions, records every call. */
function makeSeam(calls: JudgeCall[]): NonNullable<DeciderDeps["judge"]> {
	return async (state, questions) => {
		const keys = Object.keys(questions);
		const started = Date.now();
		let lastError: unknown;
		for (let attempt = 0; attempt < JUDGE_ATTEMPTS; attempt++) {
			try {
				const answers = await postToSystemOne(state, questions);
				const ms = Date.now() - started;
				calls.push({ keys, questions, state, answers, ms });
				console.error(`    [judge] ${keys.join(",")} -> ${JSON.stringify(pickAnswers(answers))} (${ms}ms)`);
				return answers;
			} catch (error) {
				lastError = error;
				if (attempt < JUDGE_ATTEMPTS - 1) {
					console.error(`    [judge] retry after: ${errorText(error)}`);
					await sleep(1_000);
				}
			}
		}
		calls.push({ keys, questions, state, answers: {}, ms: Date.now() - started, error: errorText(lastError) });
		throw lastError instanceof Error ? lastError : new Error(errorText(lastError));
	};
}

/** SystemOne may answer `noul` (0..1) even though the decider's Answers type only names `choice`. */
type SystemOneAnswer = { type?: string; choice?: string; noul?: number };

function pickAnswers(answers: Answers): Record<string, string | number> {
	const out: Record<string, string | number> = {};
	for (const [key, answer] of Object.entries(answers)) {
		const row = answer as SystemOneAnswer | undefined;
		if (row === undefined) continue;
		if (typeof row.choice === "string") out[key] = row.choice;
		else if (typeof row.noul === "number") out[key] = row.noul;
	}
	return out;
}

// ---------------------------------------------------------------------------
// Synthetic deps and scopes
// ---------------------------------------------------------------------------

function makeDeps(fixture: Fixture, judge: NonNullable<DeciderDeps["judge"]>): DeciderDeps {
	const config: OnlyneConfig = {
		path: "/tmp/judge-samples/.onlyne/config.toml",
		region: "urn:onlyne:region:lab",
		node: { id: SELF },
		server: { bind: "127.0.0.1" },
		threads: { page_size: 50 },
	};
	return {
		config,
		mainAgentId: "Main",
		contexts: {
			list: async () => fixture.contexts ?? [],
			find: async (alias: string) => (fixture.contexts ?? []).find(row => row.alias === alias),
			updateState: async () => undefined,
		} as unknown as DeciderDeps["contexts"],
		log: (message, fields) => console.error(`    [onlyne] ${message} ${JSON.stringify(fields)}`),
		listThreads: async () => fixture.threads ?? DEFAULT_THREADS,
		listJoinedThreads: async () => [],
		listMessages: async (ref: ThreadRef): Promise<Page<Entry>> => ({
			items: (fixture.messagesByThread ?? DEFAULT_MESSAGES)[ref.id] ?? [],
			next_cursor: null,
		}),
		threadMembers: async (ref: ThreadRef) => (fixture.members ?? DEFAULT_MEMBERS)[ref.id] ?? [SELF],
		listPeers: async () => fixture.peers ?? DEFAULT_PEERS,
		listTrust: async () => fixture.trust ?? DEFAULT_TRUST,
		listInbox: async () => fixture.inbox ?? [],
		listReceiverContexts: async (nodeId: NodeId) => fixture.receiverContexts?.[nodeId] ?? [],
		judge,
	};
}

/** A fake main-session OMP context; `idle` drives I-2's `is_turn_active`. */
function fakeCtx(fixture: Fixture): DecisionScope["ctx"] {
	const idle = fixture.idle ?? true;
	const branch = fixture.branch ?? DEFAULT_BRANCH;
	return {
		agent: { kind: "main", id: "Main", name: "main", depth: 0 },
		sessionManager: { getBranch: () => branch, getSessionId: () => "judge-samples" },
		isIdle: () => idle,
		hasPendingMessages: () => false,
	} as unknown as DecisionScope["ctx"];
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

type Status = "pass" | "fail" | "record-only" | "error";

interface Check {
	status: Status;
	detail: string;
}

interface RunResult {
	calls: JudgeCall[];
	action?: Action;
	inbound?: InboundDecision;
	error?: { message: string; code?: string };
	ms: number;
}

interface Scenario {
	surface: string;
	name: string;
	kind: "outbound" | "inbound";
	input?: MsgInput;
	entry?: Entry;
	plan?: DeliveryPlan;
	fixture: Fixture;
	expectation: string;
	expect: (result: RunResult) => Check;
}

const SURFACES: Array<{ id: string; title: string }> = [
	{ id: "o1", title: "O-1 intent" },
	{ id: "o2a", title: "O-2A original" },
	{ id: "relation", title: "O-2A relation" },
	{ id: "target", title: "O-2A target" },
	{ id: "response", title: "O-3A response" },
	{ id: "execution", title: "O-3A execution" },
	{ id: "receiver-context", title: "O-3A-Ctx receiver context" },
	{ id: "inbound", title: "inbound I-0/I-1/I-2" },
];

const pass = (detail: string): Check => ({ status: "pass", detail });
const fail = (detail: string): Check => ({ status: "fail", detail });
const record = (detail: string): Check => ({ status: "record-only", detail });

function choiceOf(calls: JudgeCall[], key: string): string | undefined {
	for (const call of calls) {
		if (!(key in call.questions)) continue;
		const answer = call.answers[key];
		if (answer !== undefined && typeof answer.choice === "string") return answer.choice;
	}
	return undefined;
}

function asked(calls: JudgeCall[], key: string): boolean {
	return calls.some(call => call.keys.includes(key));
}

function askedList(calls: JudgeCall[]): string {
	return calls.flatMap(call => call.keys).join(", ") || "无";
}

function errorText(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

function codeOf(error: unknown): string | undefined {
	const code = (error as { error?: { code?: unknown } } | undefined)?.error?.code;
	return typeof code === "string" ? code : undefined;
}

/** Appends what mechanically happened after the judged answer, for the record. */
function outcomeNote(result: RunResult): string {
	if (result.error !== undefined) return `下游错误: ${result.error.message}`;
	if (result.action !== undefined) return `动作: ${JSON.stringify(result.action)}`;
	if (result.inbound !== undefined) return `路由: ${JSON.stringify(result.inbound)}`;
	return "无下游结果";
}

function intentCheck(result: RunResult, expected: string): Check {
	const intent = choiceOf(result.calls, "intent");
	if (intent === undefined) return fail(`intent 未被回答;问过的问题: ${askedList(result.calls)}; ${outcomeNote(result)}`);
	return intent === expected ? pass(`intent=${intent}; ${outcomeNote(result)}`) : fail(`intent=${intent},期望 ${expected}; ${outcomeNote(result)}`);
}

function originalCheck(result: RunResult, expected: string): Check {
	const original = choiceOf(result.calls, "original");
	if (!asked(result.calls, "original")) {
		return fail(`original 未被提问;问过的问题: ${askedList(result.calls)}; ${outcomeNote(result)}`);
	}
	return original === expected
		? pass(`original=${original}; ${outcomeNote(result)}`)
		: fail(`original=${original},期望 ${expected}; ${outcomeNote(result)}`);
}

function relationCheck(result: RunResult, expected: string): Check {
	const relation = choiceOf(result.calls, "relation");
	if (!asked(result.calls, "relation")) {
		return fail(`relation 未被提问;问过的问题: ${askedList(result.calls)}; ${outcomeNote(result)}`);
	}
	return relation === expected
		? pass(`relation=${relation}; ${outcomeNote(result)}`)
		: fail(`relation=${relation},期望 ${expected}; ${outcomeNote(result)}`);
}

function targetCheck(result: RunResult, expected: NodeId): Check {
	const target = choiceOf(result.calls, "target");
	if (!asked(result.calls, "target")) {
		return fail(`target 未被提问;问过的问题: ${askedList(result.calls)}; ${outcomeNote(result)}`);
	}
	return target === expected
		? pass(`target=${target}; ${outcomeNote(result)}`)
		: fail(`target=${target},期望 ${expected}; ${outcomeNote(result)}`);
}

function responseCheck(result: RunResult, expected: string): Check {
	const response = choiceOf(result.calls, "response");
	if (!asked(result.calls, "response")) {
		return fail(`response 未被提问;问过的问题: ${askedList(result.calls)}; ${outcomeNote(result)}`);
	}
	return response === expected
		? pass(`response=${response}; ${outcomeNote(result)}`)
		: fail(`response=${response},期望 ${expected}; ${outcomeNote(result)}`);
}

function executionCheck(result: RunResult, expected: string): Check {
	const execution = choiceOf(result.calls, "execution");
	if (!asked(result.calls, "execution")) {
		return fail(`execution 未被提问;问过的问题: ${askedList(result.calls)}; ${outcomeNote(result)}`);
	}
	return execution === expected
		? pass(`execution=${execution}; ${outcomeNote(result)}`)
		: fail(`execution=${execution},期望 ${expected}; ${outcomeNote(result)}`);
}

function contextCheck(result: RunResult, allowed: string[]): Check {
	const context = choiceOf(result.calls, "context");
	if (!asked(result.calls, "context")) {
		return fail(`context 未被提问;问过的问题: ${askedList(result.calls)}; ${outcomeNote(result)}`);
	}
	return context !== undefined && allowed.includes(context)
		? pass(`context=${context}; ${outcomeNote(result)}`)
		: fail(`context=${context},期望 ${allowed.join(" 或 ")}; ${outcomeNote(result)}`);
}

function needsAnswerCheck(result: RunResult, expected: string): Check {
	const answer = choiceOf(result.calls, "needs_answer");
	if (!asked(result.calls, "needs_answer")) {
		return fail(`needs_answer 未被提问;问过的问题: ${askedList(result.calls)}; ${outcomeNote(result)}`);
	}
	return answer === expected
		? pass(`needs_answer=${answer}; ${outcomeNote(result)}`)
		: fail(`needs_answer=${answer},期望 ${expected}; ${outcomeNote(result)}`);
}

function insertionCheck(result: RunResult, allowed: string[]): Check {
	const mode = choiceOf(result.calls, "insertion_mode");
	if (mode === undefined) {
		return fail(
			`insertion_mode 未被提问(needs_answer=${choiceOf(result.calls, "needs_answer") ?? "未问"},`
			+ ` routing_target=${choiceOf(result.calls, "routing_target") ?? "未问"}); ${outcomeNote(result)}`,
		);
	}
	return allowed.includes(mode)
		? pass(`insertion_mode=${mode}; ${outcomeNote(result)}`)
		: fail(`insertion_mode=${mode},期望 ${allowed.join(" 或 ")}; ${outcomeNote(result)}`);
}

function routingCheck(result: RunResult, expected: string): Check {
	const routing = choiceOf(result.calls, "routing_target");
	if (routing === undefined) {
		return fail(`routing_target 未被提问(needs_answer=${choiceOf(result.calls, "needs_answer") ?? "未问"}); ${outcomeNote(result)}`);
	}
	return routing === expected
		? pass(`routing_target=${routing}; ${outcomeNote(result)}`)
		: fail(`routing_target=${routing},期望 ${expected}; ${outcomeNote(result)}`);
}

const RELATION_FIXTURE: Fixture = { inbox: [{ attention: { entry: E_RELEASE_ASK } }] };
const TWO_CANDIDATES: InboxRow[] = [{ attention: { entry: E_RELEASE_ASK } }, { attention: { entry: E_DATASET_ASK } }];
const CAERULEA_MAIN_RC: Record<NodeId, ReceiverContext[]> = { [CAERULEA]: [RC_MAIN] };
const CAERULEA_THREE_RC: Record<NodeId, ReceiverContext[]> = { [CAERULEA]: [RC_MAIN, RC_MIGRATION, RC_DOCS] };

const SCENARIOS: Scenario[] = [
	// ---- O-1 intent ---------------------------------------------------------
	{
		surface: "o1",
		name: "o1-communicate",
		kind: "outbound",
		input: {
			delivery_kind: "tell caerulea about the maintenance move",
			msg: "帮我把构建日志发给 caerulea,并告诉他今晚的网关维护提前到 22:00。",
			meta: { to: "caerulea", response: "none", execution: "new" },
		},
		fixture: {},
		expectation: 'intent == "communicate"',
		expect: result => intentCheck(result, "communicate"),
	},
	{
		surface: "o1",
		name: "o1-inspect",
		kind: "outbound",
		input: {
			delivery_kind: "see who is online",
			msg: "看看现在谁在线,列一下邻居节点。",
			meta: {},
		},
		fixture: {},
		expectation: 'intent == "inspect"',
		expect: result => intentCheck(result, "inspect"),
	},
	{
		surface: "o1",
		name: "o1-manage-thread",
		kind: "outbound",
		input: {
			delivery_kind: "create a thread for deploy notes",
			msg: "帮我新建一个叫 deploy-notes 的会话线程,以后部署纪要都记在那里。",
			meta: { title: "deploy-notes" },
		},
		fixture: {},
		expectation: 'intent == "manage_thread"',
		expect: result => intentCheck(result, "manage_thread"),
	},
	{
		surface: "o1",
		name: "o1-manage-domain",
		kind: "outbound",
		input: {
			delivery_kind: "show our domain membership",
			msg: "看看我们节点现在加入了哪些域,描述一下成员身份。",
			meta: {},
		},
		fixture: {},
		expectation: 'intent == "manage_domain"',
		expect: result => intentCheck(result, "manage_domain"),
	},
	{
		surface: "o1",
		name: "o1-operate-resource",
		kind: "outbound",
		input: {
			delivery_kind: "restart the im gateway resource",
			msg: "对资源 im-gateway-1 执行重启操作。",
			// A complete meta.resource plus operation is how a resource operation reaches the
			// plugin, and `#explicit` handles it before O-1: a partial meta.resource is refused
			// mechanically and an absent one is steered away by the O-1 criteria themselves, so
			// the judged operate_resource branch is unreachable and this documents that path.
			meta: { resource: { owner: SELF, id: "im-gateway-1" }, operation: "restart" },
		},
		fixture: {},
		expectation: "机械断言:完整 meta.resource + operation 走显式路径,O-1 不应被提问",
		expect: result => (result.calls.length === 0 && result.action?.kind === "resource"
			? pass(`显式资源动作 ${JSON.stringify(result.action)} ,未消耗 judge 调用`)
			: fail(`期望显式资源动作且无 judge 调用;实际 judge 调用 ${result.calls.length} 次,${outcomeNote(result)}`)),
	},

	// ---- O-2A original ------------------------------------------------------
	{
		surface: "o2a",
		name: "original-single-answer",
		kind: "outbound",
		input: {
			delivery_kind: "answer caerulea's parameter question",
			msg: "确认:第二组参数按 42 执行,配置已更新,回归测试全绿。",
			meta: { response: "async", execution: "new" },
		},
		fixture: { inbox: [{ attention: { entry: E_PARAMS_ASK } }] },
		expectation: 'original == "answer_1"(唯一候选,明显在回答它)',
		expect: result => originalCheck(result, "answer_1"),
	},
	{
		surface: "o2a",
		name: "original-multi-named",
		kind: "outbound",
		input: {
			delivery_kind: "reply to midnight's question",
			msg: "回复 midnight:数据集 v2 的校验和是 9f86d08188,迁移脚本可以直接用了。",
			meta: { response: "async", execution: "new" },
		},
		fixture: { inbox: TWO_CANDIDATES },
		expectation: 'original == "answer_2"(正文点名 midnight,answer_2 是 midnight 的候选)',
		expect: result => originalCheck(result, "answer_2"),
	},
	{
		surface: "o2a",
		name: "original-new-topic",
		kind: "outbound",
		input: {
			delivery_kind: "start a readme task with junco",
			msg: "启动一件全新的事:帮我把仓库 README 的安装步骤重写成 bun 的一键安装命令。",
			meta: { to: "junco", response: "async", execution: "new" },
		},
		fixture: { inbox: TWO_CANDIDATES },
		expectation: 'original == "new_message"(与两个候选的话题都无关的新任务)',
		expect: result => originalCheck(result, "new_message"),
	},
	{
		surface: "o2a",
		name: "original-greet-question",
		kind: "outbound",
		input: {
			delivery_kind: "answer junco's greeting",
			msg: "你好!周五的评审我可以参加,材料我提前一天发给你。",
			meta: { response: "async", execution: "new" },
		},
		fixture: { inbox: [{ attention: { entry: E_GREET_ASK } }] },
		expectation: 'original == "answer_1"(候选里的问候+提问,回答回到它)',
		expect: result => originalCheck(result, "answer_1"),
	},
	{
		surface: "o2a",
		name: "original-status-note",
		kind: "outbound",
		input: {
			delivery_kind: "acknowledge midnight's report",
			msg: "收到,感谢同步。",
			meta: { to: "midnight", response: "none", execution: "new" },
		},
		fixture: { inbox: [{ attention: { entry: E_PROGRESS_NOTE } }] },
		expectation: "record-only:纯状态通报的归属本身有歧义(回答或新消息都说得通),只记录不判定",
		expect: result => record(`original=${choiceOf(result.calls, "original") ?? "(未问)"}; ${outcomeNote(result)}`),
	},
	{
		surface: "o2a",
		name: "original-no-candidates",
		kind: "outbound",
		input: {
			delivery_kind: "send progress to junco",
			msg: "给 junco 发一条消息,同步一下我们这边的迁移进度。",
			meta: { to: "junco", response: "none", execution: "new" },
		},
		fixture: { inbox: [] },
		expectation: "机械断言:无候选时 original 不应被提问(不经 judge)",
		expect: result => (asked(result.calls, "original") ? fail(`original 被提问了,但候选集为空;问过: ${askedList(result.calls)}`) : pass(`original 未被提问; ${outcomeNote(result)}`)),
	},
	{
		surface: "o2a",
		name: "original-other-context",
		kind: "outbound",
		input: {
			delivery_kind: "confirm the parameter question",
			msg: "确认:第二组参数按 42 执行。",
			meta: { to: "caerulea", response: "async", execution: "new" },
		},
		fixture: { inbox: [{ context: "ctx-1", attention: { entry: E_PARAMS_ASK } }] },
		expectation: "机械断言:候选投递到了别的上下文(ctx-1),主会话不应把它列为候选,original 不应被提问",
		expect: result => (asked(result.calls, "original") ? fail(`original 被提问了,但候选属于 ctx-1;问过: ${askedList(result.calls)}`) : pass(`original 未被提问; ${outcomeNote(result)}`)),
	},
	{
		surface: "o2a",
		name: "original-multi-ambiguous",
		kind: "outbound",
		input: {
			delivery_kind: "acknowledge both messages",
			msg: "收到,都看到了,我这边处理一下。",
			meta: { to: "caerulea", response: "none", execution: "new" },
		},
		fixture: { inbox: TWO_CANDIDATES },
		expectation: "record-only:多候选且无明确指涉,选谁或选新消息都缺乏判据,只记录",
		expect: result => record(`original=${choiceOf(result.calls, "original") ?? "(未问)"}; ${outcomeNote(result)}`),
	},

	// ---- relation -----------------------------------------------------------
	{
		surface: "relation",
		name: "relation-final-reply",
		kind: "outbound",
		input: {
			delivery_kind: "answer the release-time question",
			msg: "确认:发布时间定在周五 15:00,公告我这边已经写好了。",
			meta: { in_reply_to: { sender: "caerulea", id: "m-release-1" }, response: "async", execution: "new" },
		},
		fixture: RELATION_FIXTURE,
		expectation: 'relation == "reply"(最终答复,可解除对方的等待)',
		expect: result => relationCheck(result, "reply"),
	},
	{
		surface: "relation",
		name: "relation-progress-followup",
		kind: "outbound",
		input: {
			delivery_kind: "supplement the release thread",
			msg: "补充一下:公告里的下载链接还要等 CI 出包,我拿到链接后贴过来。",
			meta: { in_reply_to: { sender: "caerulea", id: "m-release-1" }, response: "async", execution: "new" },
		},
		fixture: RELATION_FIXTURE,
		expectation: 'relation == "followup"(进度补充,不解除等待)',
		expect: result => relationCheck(result, "followup"),
	},
	{
		surface: "relation",
		name: "relation-refer-ambiguous",
		kind: "outbound",
		input: {
			delivery_kind: "acknowledge briefly",
			msg: "好。",
			meta: { in_reply_to: { sender: "caerulea", id: "m-release-1" }, response: "none", execution: "new" },
		},
		fixture: RELATION_FIXTURE,
		expectation: "record-only:引用了消息但只有一个字的确认,reply/followup 均说得通,只记录",
		expect: result => record(`relation=${choiceOf(result.calls, "relation") ?? "(未问)"}; ${outcomeNote(result)}`),
	},

	// ---- target -------------------------------------------------------------
	{
		surface: "target",
		name: "target-short-name",
		kind: "outbound",
		input: {
			delivery_kind: "notify the maintenance change",
			msg: "把今晚网关维护提前到 22:00 这件事告诉 caerulea。",
			meta: { response: "none", execution: "new" },
		},
		fixture: {},
		expectation: `target == ${CAERULEA}(正文点名短名)`,
		expect: result => targetCheck(result, CAERULEA),
	},
	{
		surface: "target",
		name: "target-full-name",
		kind: "outbound",
		input: {
			delivery_kind: "send the checksum over",
			msg: "把数据集 v2 的校验和 9f86d08188 发给 urn:onlyne:agent:midnight。",
			meta: { response: "none", execution: "new" },
		},
		fixture: {},
		expectation: `target == ${MIDNIGHT}(正文点名全名)`,
		expect: result => targetCheck(result, MIDNIGHT),
	},
	{
		surface: "target",
		name: "target-unnamed",
		kind: "outbound",
		input: {
			delivery_kind: "share the minutes",
			msg: "把会议纪要发给相关的人。",
			meta: { response: "none", execution: "new" },
		},
		fixture: {},
		expectation: "record-only:三个 peer 且正文无人名,目标缺乏判据,只记录",
		expect: result => record(`target=${choiceOf(result.calls, "target") ?? "(未问)"}; ${outcomeNote(result)}`),
	},

	// ---- response -----------------------------------------------------------
	{
		surface: "response",
		name: "response-none",
		kind: "outbound",
		input: {
			delivery_kind: "post a maintenance notice",
			msg: "通知:今晚 22:00-23:00 网关例行维护,期间消息投递会延迟,不用回复。",
			meta: { to: "caerulea", execution: "new" },
		},
		fixture: {},
		expectation: 'response == "none"(接收方不必动手)',
		expect: result => responseCheck(result, "none"),
	},
	{
		surface: "response",
		name: "response-async",
		kind: "outbound",
		input: {
			delivery_kind: "dispatch a staging deploy",
			msg: "caerulea,麻烦把新的构建产物部署到预发环境,我这边继续写发布说明,部署好了告诉我一声就行。",
			meta: { to: "caerulea", execution: "new" },
		},
		fixture: {},
		expectation: 'response == "async"(对方要动手,但我能继续)',
		expect: result => responseCheck(result, "async"),
	},
	{
		surface: "response",
		name: "response-await",
		kind: "outbound",
		input: {
			delivery_kind: "request a blocking confirmation",
			msg: "等你的确认:预发环境的探活结果出来了吗?我必须拿到结果才能切线上流量。",
			meta: { to: "caerulea", execution: "new" },
		},
		fixture: {},
		expectation: 'response == "await"(下一步必须等答复)',
		expect: result => responseCheck(result, "await"),
	},

	// ---- execution ----------------------------------------------------------
	{
		surface: "execution",
		name: "execution-continues-reply",
		kind: "outbound",
		input: {
			delivery_kind: "continue the release work",
			msg: "接着刚才的发布任务说:第一批冒烟测试全部通过,继续按原计划推进第二轮。",
			meta: { in_reply_to: { sender: "caerulea", id: "m-release-1" }, relation: "reply", response: "async" },
		},
		fixture: { inbox: RELATION_FIXTURE.inbox, receiverContexts: CAERULEA_MAIN_RC },
		expectation: 'execution == "existing"(isReply 变体:延续被引用消息的工作)',
		expect: result => executionCheck(result, "existing"),
	},
	{
		surface: "execution",
		name: "execution-fresh-task",
		kind: "outbound",
		input: {
			delivery_kind: "start an archival task",
			msg: "junco,交给你一个新差事:把去年全年的构建日志归档到冷存储,和手头的事无关。",
			meta: { to: "junco", response: "async" },
		},
		fixture: {},
		expectation: 'execution == "new"(非 reply 变体:全新独立任务)',
		expect: result => executionCheck(result, "new"),
	},
	{
		surface: "execution",
		name: "execution-reply-but-new-work",
		kind: "outbound",
		input: {
			delivery_kind: "raise an unrelated request",
			msg: "另外,新需求:帮我在 junco 节点上申请一个接口测试用的证书,和刚才消息里的事完全无关,单独开工。",
			meta: { in_reply_to: { sender: "caerulea", id: "m-release-1" }, relation: "reply", response: "async" },
		},
		fixture: { inbox: RELATION_FIXTURE.inbox, receiverContexts: CAERULEA_MAIN_RC },
		expectation: 'execution == "new"(isReply 变体下内容却是无关新工作,考验 criteria 差异)',
		expect: result => executionCheck(result, "new"),
	},

	// ---- receiver context ---------------------------------------------------
	{
		surface: "receiver-context",
		name: "receiver-context-existing",
		kind: "outbound",
		input: {
			delivery_kind: "continue the migration work",
			msg: "迁移任务有更新:第一批分片校验完成,继续在迁移这条线上推进第二批。",
			meta: { to: "caerulea", response: "async", execution: "existing" },
		},
		fixture: { receiverContexts: CAERULEA_THREE_RC },
		expectation: 'context == "migration"(延续已发布且在跑的迁移上下文)',
		expect: result => contextCheck(result, ["migration"]),
	},
	{
		surface: "receiver-context",
		name: "receiver-context-fresh",
		kind: "outbound",
		input: {
			delivery_kind: "ask for a disk check",
			msg: "另外有一件全新的事拜托 caerulea:帮忙看看你们节点的磁盘水位,和迁移、文档两个任务都无关,结果回我。",
			meta: { to: "caerulea", response: "async", execution: "existing" },
		},
		fixture: { receiverContexts: CAERULEA_THREE_RC },
		expectation: 'context ∈ {none, main}(全新工作不延续任何上下文;none 由机制回落到 main)',
		expect: result => contextCheck(result, ["none", "main"]),
	},
	{
		surface: "receiver-context",
		name: "receiver-context-ambiguous",
		kind: "outbound",
		input: {
			delivery_kind: "check in on their work",
			msg: "跟一下你们那边的进展。",
			meta: { to: "caerulea", response: "none", execution: "existing" },
		},
		fixture: { receiverContexts: CAERULEA_THREE_RC },
		expectation: "record-only:对方有 running 上下文但正文无法定位是哪一个,只记录",
		expect: result => record(`context=${choiceOf(result.calls, "context") ?? "(未问)"}; ${outcomeNote(result)}`),
	},

	// ---- inbound: needs_answer / routing / insertion ------------------------
	{
		surface: "inbound",
		name: "inbound-ask-needs-answer",
		kind: "inbound",
		entry: E_DATASET_ASK,
		fixture: { contexts: [CTX_MIGRATION, CTX_DOCS] },
		expectation: 'needs_answer == "yes"(直接提问)',
		expect: result => needsAnswerCheck(result, "yes"),
	},
	{
		surface: "inbound",
		name: "inbound-status-no-answer",
		kind: "inbound",
		entry: E_STATUS_SYNC,
		fixture: { contexts: [CTX_MIGRATION, CTX_DOCS] },
		expectation: 'needs_answer == "no"(纯状态通报)',
		expect: result => needsAnswerCheck(result, "no"),
	},
	{
		surface: "inbound",
		name: "inbound-urgent-steer",
		kind: "inbound",
		entry: makeEntry("t-data", "3", MIDNIGHT, "紧急修正", "紧急:停下当前部署,立刻回滚到上一个版本!回滚完回复我确认。", "m-stop-1"),
		fixture: { idle: false },
		expectation: 'insertion_mode == "steer"(紧急修正须打断当前轮)',
		expect: result => insertionCheck(result, ["steer"]),
	},
	{
		surface: "inbound",
		name: "inbound-supplement-follow-up",
		kind: "inbound",
		entry: makeEntry("t-data", "4", MIDNIGHT, "补充需求", "补充:迁移需求里再加一条,移动端也要支持深色模式,不急,忙完手头的事再看。", "m-supp-1"),
		fixture: { idle: true },
		expectation: 'insertion_mode == "follow_up"(可等的补充,当前轮结束后开新轮)',
		expect: result => insertionCheck(result, ["follow_up"]),
	},
	{
		surface: "inbound",
		name: "inbound-idle-fragment",
		kind: "inbound",
		entry: makeEntry("t-research", "4", JUNCO, "状态同步", "这是发给主会话的状态同步,不需要任何后续处理:CI 全部通过,版本 v3.0.1 已归档,暂时不用管。", "m-arch-1"),
		fixture: { idle: true, contexts: [CTX_DOCS] },
		expectation: 'insertion_mode == "fragment"(主会话 idle 的状态投递,不开启轮次)',
		expect: result => insertionCheck(result, ["fragment"]),
	},
	{
		surface: "inbound",
		name: "inbound-running-delivery",
		kind: "inbound",
		entry: makeEntry("t-release", "2", CAERULEA, "提醒", "提醒:构建完成了,把产物链接发到发布线程,我等着贴发布说明。", "m-remind-1"),
		fixture: { idle: false },
		expectation: "insertion_mode ∈ {steer, follow_up}(投递到达时工作在跑;非紧急,倾向 follow_up)",
		expect: result => insertionCheck(result, ["steer", "follow_up"]),
	},
	{
		surface: "inbound",
		name: "inbound-subagent-task",
		kind: "inbound",
		entry: makeEntry("t-data", "3", MIDNIGHT, "任务派发", "请整理仓库近三个月的全部 issue,按主题聚类,输出一份带引用来源的报告;报告直接写入知识库即可,不用发给我,也不用回复这条消息。", "m-issue-1"),
		fixture: { contexts: [CTX_MIGRATION, CTX_DOCS] },
		expectation: 'routing_target == "new_subagent"(自成一体的重任务,不需答复,与现有上下文都无关)',
		expect: result => routingCheck(result, "new_subagent"),
	},
	{
		surface: "inbound",
		name: "inbound-context-lost",
		kind: "inbound",
		entry: E_DATASET_ASK,
		plan: { response: "none", execution: "existing", context: "c-9" },
		fixture: { contexts: [CTX_LOST] },
		expectation: "机械断言:plan 点名的上下文已 lost,decideInbound 应抛 context_lost 且不问 judge",
		expect: result => (result.error?.code === "context_lost" && result.calls.length === 0
			? pass(`context_lost 已抛出,未消耗 judge 调用`)
			: fail(`期望 context_lost 错误且无 judge 调用;实际 ${result.error?.code ?? "(无错误)"},judge 调用 ${result.calls.length} 次`)),
	},
];

// ---------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------

interface OutcomeJson {
	surface: string;
	surface_title: string;
	name: string;
	kind: string;
	expectation: string;
	status: Status;
	detail: string;
	ms: number;
	input?: MsgInput;
	entry?: Entry;
	plan?: DeliveryPlan;
	answers: Record<string, string | number | undefined>;
	action?: Action;
	inbound?: InboundDecision;
	error?: { message: string; code?: string };
	calls: Array<{
		keys: string[];
		questions?: Questions;
		state?: Record<string, unknown>;
		answers: Answers;
		ms: number;
		error?: string;
	}>;
}

async function runScenario(scenario: Scenario): Promise<OutcomeJson> {
	const calls: JudgeCall[] = [];
	const decider = new JudgeDecider(makeDeps(scenario.fixture, makeSeam(calls)));
	const result: RunResult = { calls, ms: 0 };
	const started = Date.now();
	try {
		if (scenario.kind === "outbound") {
			result.action = await decider.decide({ ctx: fakeCtx(scenario.fixture), input: scenario.input as MsgInput });
		} else {
			result.inbound = await decider.decideInbound(fakeCtx(scenario.fixture), scenario.entry as Entry, scenario.plan);
		}
	} catch (error) {
		result.error = { message: errorText(error), code: codeOf(error) };
	}
	result.ms = Date.now() - started;

	let check: Check;
	try {
		check = scenario.expect(result);
	} catch (error) {
		check = { status: "error", detail: `期望函数本身出错: ${errorText(error)}` };
	}

	const keepAll = check.status !== "pass";
	const answers: Record<string, string | number | undefined> = {};
	for (const call of calls) for (const [key, value] of Object.entries(pickAnswers(call.answers))) answers[key] = value;

	return {
		surface: scenario.surface,
		surface_title: SURFACES.find(row => row.id === scenario.surface)?.title ?? scenario.surface,
		name: scenario.name,
		kind: scenario.kind,
		expectation: scenario.expectation,
		status: check.status,
		detail: check.detail,
		ms: result.ms,
		...(scenario.input === undefined ? {} : { input: scenario.input }),
		...(scenario.entry === undefined ? {} : { entry: scenario.entry }),
		...(scenario.plan === undefined ? {} : { plan: scenario.plan }),
		answers,
		...(result.action === undefined ? {} : { action: result.action }),
		...(result.inbound === undefined ? {} : { inbound: result.inbound }),
		...(result.error === undefined ? {} : { error: result.error }),
		calls: calls.map(call => ({
			keys: call.keys,
			answers: call.answers,
			ms: call.ms,
			...(call.error === undefined ? {} : { error: call.error }),
			...(keepAll ? { questions: call.questions } : {}),
			...(keepAll ? { state: call.state } : {}),
		})),
	};
}

function summarize(outcomes: OutcomeJson[]): Array<{
	surface: string;
	title: string;
	total: number;
	pass: number;
	fail: number;
	record_only: number;
	error: number;
	rate: string;
}> {
	return SURFACES
		.map(({ id, title }) => {
			const rows = outcomes.filter(outcome => outcome.surface === id);
			const count = (status: Status): number => rows.filter(row => row.status === status).length;
			const judged = count("pass") + count("fail") + count("error");
			return {
				surface: id,
				title,
				total: rows.length,
				pass: count("pass"),
				fail: count("fail"),
				record_only: count("record-only"),
				error: count("error"),
				rate: judged === 0 ? "n/a" : `${count("pass")}/${judged}`,
			};
		})
		.filter(row => row.total > 0);
}

async function main(): Promise<void> {
	let only: string | undefined;
	let out: string | undefined;
	for (const arg of process.argv.slice(2)) {
		if (arg.startsWith("--only=")) only = arg.slice("--only=".length);
		else if (arg.startsWith("--out=")) out = arg.slice("--out=".length);
	}
	// A filtered run keeps its own artifact instead of clobbering the full results file.
	const outPath = out ?? (only === undefined ? DEFAULT_OUT : `/tmp/judge-samples/omp-results-${only}.json`);

	console.error(`== probe ${ENDPOINT} (model ${MODEL})`);
	try {
		const answers = await postToSystemOne(
			{ probe: "onlyne judge-samples connectivity probe" },
			{ ping: { type: "choice", instructions: "Connectivity probe for the judge-sample runner. Answer ping.", criteria: { ping: "Always the answer for this probe." } } },
		);
		const ping = answers.ping?.choice;
		if (typeof ping !== "string") throw new Error(`probe answered ${JSON.stringify(answers)}`);
		console.error(`   endpoint ok, probe answer: ${ping}`);
	} catch (error) {
		console.error(`SystemOne judge endpoint unreachable at ${ENDPOINT}: ${errorText(error)}`);
		process.exit(1);
	}

	const scenarios = SCENARIOS.filter(scenario => only === undefined || scenario.surface === only);
	const outcomes: OutcomeJson[] = [];
	for (const [index, scenario] of scenarios.entries()) {
		console.error(`[${String(index + 1).padStart(2, "0")}/${String(scenarios.length)}] ${scenario.surface}/${scenario.name}`);
		const outcome = await runScenario(scenario);
		outcomes.push(outcome);
		console.error(`    -> ${outcome.status} (${outcome.ms}ms) ${outcome.detail}`);
	}

	const summary = summarize(outcomes);
	const totals = summary.reduce(
		(acc, row) => ({ pass: acc.pass + row.pass, fail: acc.fail + row.fail, record_only: acc.record_only + row.record_only, error: acc.error + row.error }),
		{ pass: 0, fail: 0, record_only: 0, error: 0 },
	);
	const judgedTotal = totals.pass + totals.fail + totals.error;

	console.log(`\n== 通过率(按判定面;通过率不含 record-only)`);
	console.log(
		"surface".padEnd(20) + "total".padStart(6) + "pass".padStart(6) + "fail".padStart(6) + "record-only".padStart(12) + "error".padStart(7) + "  通过率",
	);
	for (const row of summary) {
		console.log(
			`${(row.title + ` (${row.surface})`).padEnd(20)}`
			+ String(row.total).padStart(6) + String(row.pass).padStart(6) + String(row.fail).padStart(6)
			+ String(row.record_only).padStart(12) + String(row.error).padStart(7) + `  ${row.rate}`,
		);
	}
	console.log(
		`${"total".padEnd(20)}`
		+ String(outcomes.length).padStart(6) + String(totals.pass).padStart(6) + String(totals.fail).padStart(6)
		+ String(totals.record_only).padStart(12) + String(totals.error).padStart(7)
		+ `  ${judgedTotal === 0 ? "n/a" : `${totals.pass}/${judgedTotal}`}`,
	);

	const notable = outcomes.filter(outcome => outcome.status !== "pass");
	if (notable.length === 0) {
		console.log("\n== 全部场景通过,无不匹配项");
	} else {
		console.log(`\n== 不匹配 / 记录项原文(${notable.length} 个)`);
		for (const outcome of notable) {
			console.log(`\n--- [${outcome.status}] ${outcome.surface}/${outcome.name}`);
			console.log(`期望: ${outcome.expectation}`);
			console.log(`判定: ${outcome.detail}`);
			if (outcome.error?.message !== undefined) console.log(`错误: ${outcome.error.message}`);
			for (const call of outcome.calls) {
				console.log(`  调用 [${call.keys.join(", ")}] ${call.ms}ms${call.error === undefined ? "" : ` 失败(已重试): ${call.error}`}`);
				for (const [key, question] of Object.entries(call.questions ?? {})) {
					console.log(`    ${key} instructions: ${question.instructions}`);
					for (const [option, criterion] of Object.entries(question.criteria)) console.log(`      ${option}: ${criterion}`);
					console.log(`      -> 模型回答: ${JSON.stringify(call.answers[key] ?? null)}`);
				}
				if (call.state !== undefined) console.log(`    state(截断): ${JSON.stringify(call.state).slice(0, 2_000)}`);
			}
		}
	}

	const report = {
		generated_at: new Date().toISOString(),
		endpoint: ENDPOINT,
		model: MODEL,
		total: outcomes.length,
		pass: totals.pass,
		fail: totals.fail,
		record_only: totals.record_only,
		error: totals.error,
		surfaces: summary,
		scenarios: outcomes,
	};
	mkdirSync(dirname(outPath), { recursive: true });
	await writeFile(outPath, `${JSON.stringify(report, null, 2)}\n`);
	console.log(`\n结果已写入 ${outPath}`);
}

const sleep = (ms: number): Promise<void> => new Promise(resolve => setTimeout(resolve, ms));

await main();
