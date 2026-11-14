import { describe, expect, test } from "bun:test";
import type { Snapshot } from "../src/core/types.ts";
import { validateBootstrapValues } from "../src/tui/bootstrap.ts";
import { truncateWidth, visibleWidth, wrapWidth } from "../src/tui/format.ts";
import { applySnapshot, createInitialTuiState } from "../src/tui/state.ts";
import { renderTuiFrame } from "../src/tui/render.ts";
import { decodeKeyEvents, dispatchTuiKey, type TuiDaemonActions } from "../src/tui/app.ts";

function sampleSnapshot(): Snapshot {
	return {
		at: "2026-11-14T10:02:00.000Z",
		workspace: "/tmp/ws",
		configured: true,
		language: "en",
		fingerprint: "fp-local-1234567890abcdef",
		judge_mode: "auto",
		acquisition: { policy: "segment", allow: ["127.0.0.1/32"], allow_loopback: true },
		audit: [],
		todos: [
			{
				id: "td-1",
				phase: "Verify",
				title: "Verify remote build",
				status: "pending",
				source_sender: "urn:onlyne:agent:peer-b",
				source_id: "m-1",
				thread_key: "peer-b/t-1",
				updated_at: "2026-11-14T10:00:00.000Z",
			},
		],
		pending_deliveries: [],
		status: {
			node_id: "urn:onlyne:agent:agy-local",
			region: "urn:onlyne:region:local",
			endpoint: "http://127.0.0.1:7801/onlyne/v3",
			domains: ["dom-core"],
			peers_total: 1,
			peers_reachable: 1,
			trusted: 1,
			pending_trust: 0,
			pending_keys: 0,
			pending_approvals: 0,
			unread: 1,
			inbox_count: 1,
			outbound_pending: 0,
			contexts_active: 1,
			decisions_pending: 0,
		},
		domains: [
			{
				domain_id: "dom-core",
				state: "active",
				revision: "1",
				members: ["urn:onlyne:agent:agy-local", "urn:onlyne:agent:peer-b"],
				updated_at: "2026-11-14T10:00:00.000Z",
			},
		],
		threads: [
			{
				ref: { owner: "urn:onlyne:agent:agy-local", id: "t-1" },
				alias: "agy-local/t-1",
				title: "Integration Sync",
				read_only: false,
				state: "open",
				unread: 0,
				last_seq: "1",
				messages: [
					{
						ref: { sender: "urn:onlyne:agent:peer-b", id: "m-0" },
						seq: "1",
						direction: "in",
						author: "urn:onlyne:agent:peer-b",
						text: "中文测试与 English line wrapping cleanly across columns",
						at: "2026-11-14T10:01:00.000Z",
					},
				],
			},
		],
		trust: [
			{
				node_id: "urn:onlyne:agent:peer-b",
				phase: "exchanged",
				fingerprint: "fp-peer-b-9876543210",
				url: "http://127.0.0.1:7802/onlyne/v3",
				verified: true,
				updated_at: "2026-11-14T10:01:00.000Z",
			},
		],
		discovery: [
			{
				node_id: "urn:onlyne:agent:peer-b",
				instance_id: "inst-b",
				region: "urn:onlyne:region:local",
				base_url: "http://127.0.0.1:7802/onlyne/v3",
				source: "seed",
				expires_at: "2026-11-14T12:00:00.000Z",
				last_seen: "2026-11-14T10:01:00.000Z",
			},
		],
		contacts: [],
		queue: [],
		contexts: [
			{
				alias: "main",
				node_id: "urn:onlyne:agent:agy-local",
				main_agent_id: "antigravity-main",
				agent_id: "main",
				summary: "Main session",
				state: "running",
				created_at: "2026-11-14T10:00:00.000Z",
			},
		],
	};
}

function makeMockActions(snap: Snapshot, calls: string[], extra?: Partial<TuiDaemonActions>): TuiDaemonActions {
	return {
		connect: async url => { calls.push(`connect:${url}`); return {}; },
		refresh: async () => { calls.push("refresh"); return snap; },
		trustBegin: async nodeId => {
			calls.push(`trustBegin:${nodeId}`);
			return { node_id: nodeId, base_url: "http://127.0.0.1:7802/onlyne/v3", fingerprint: "fp-peer-b-9876543210" };
		},
		trustConfirm: async (nodeId, fp) => { calls.push(`trustConfirm:${nodeId}:${fp}`); return {}; },
		trustAccept: async nodeId => { calls.push(`trustAccept:${nodeId}`); return {}; },
		trustRevoke: async nodeId => { calls.push(`trustRevoke:${nodeId}`); return {}; },
		trustRotate: async () => { calls.push("trustRotate"); return { fingerprint: "fp-rotated" }; },
		setAcquisition: async (policy, allow) => { calls.push(`setAcq:${policy}:${allow.join(",")}`); return {}; },
		setLanguage: async language => { calls.push(`setLang:${language}`); return {}; },
		send: async payload => {
			calls.push(`send:${payload.delivery_kind ?? "note"}:${payload.msg}:${JSON.stringify(payload)}`);
			return {};
		},
		domainAction: async (op, domainId) => { calls.push(`domain:${op}:${domainId}`); return {}; },
		createThread: async title => { calls.push(`createThread:${title}`); return {}; },
		updateTodo: async (id, status) => { calls.push(`todo:${id}:${status}`); return {}; },
		saveConfig: async payload => { calls.push(`saveConfig:${String(payload.node_id)}`); return { ok: true, snapshot: snap }; },
		snapshot: async () => snap,
		...extra,
	};
}

describe("Standalone TUI formatter, layout, key decoder & state transitions", () => {
	test("decodes terminal escape sequences, control keys, and UTF-8 text", () => {
		const keys = decodeKeyEvents("\x1b[A\x1b[B\x1b[Z\t\x13\x03你好");
		expect(keys.map(k => ({ name: k.name, ctrl: Boolean(k.ctrl), shift: Boolean(k.shift), char: k.char }))).toEqual([
			{ name: "up", ctrl: false, shift: false, char: undefined },
			{ name: "down", ctrl: false, shift: false, char: undefined },
			{ name: "tab", ctrl: false, shift: true, char: undefined },
			{ name: "tab", ctrl: false, shift: false, char: undefined },
			{ name: "s", ctrl: true, shift: false, char: undefined },
			{ name: "c", ctrl: true, shift: false, char: undefined },
			{ name: "你", ctrl: false, shift: false, char: "你" },
			{ name: "好", ctrl: false, shift: false, char: "好" },
		]);
	});

	test("computes CJK wide-char widths, truncates ANSI strings safely, and renders wide + compact frames", () => {
		expect(visibleWidth("ab中文")).toBe(6);
		expect(visibleWidth("\x1b[36m中文\x1b[0m")).toBe(4);
		expect(visibleWidth(truncateWidth("中文宽度截断测试", 6))).toBeLessThanOrEqual(6);
		expect(wrapWidth("Hello 世界 Onlyne", 8).every(line => visibleWidth(line) <= 8)).toBe(true);

		const snap = sampleSnapshot();
		const state = applySnapshot(createInitialTuiState("en"), snap);

		// Wide 4-column rendering
		const wide = renderTuiFrame(state, 120, 32, { color: false });
		expect(wide).toContain("Domains");
		expect(wide).toContain("Workspace");
		expect(wide).toContain("Conversation");
		expect(wide).toContain("Peers");
		expect(wide).toContain("Integration Sync");

		// Compact (< 96 cols) tab rendering
		const compact = renderTuiFrame({ ...state, activePane: "members" }, 76, 26, { color: false });
		expect(compact).toContain("4:Peers");
		expect(compact).toContain("Peer Detail: peer-b");
	});

	test("handles compose+Ctrl+S, interactive key exchange confirmation, language toggle, and bootstrap validation", async () => {
		const snap = sampleSnapshot();
		const calls: string[] = [];
		const actions = makeMockActions(snap, calls);
		let state = applySnapshot(createInitialTuiState("en"), snap);
		const update = (fn: (cur: typeof state) => typeof state): void => {
			state = fn(state);
		};

		// Open compose with 'c', type text, send with Ctrl+S
		state = dispatchTuiKey(state, { name: "c", char: "c" }, "/tmp/ws", actions, update);
		expect(state.modal?.kind).toBe("compose");
		for (const ch of ["H", "i"]) {
			state = dispatchTuiKey(state, { name: ch, char: ch }, "/tmp/ws", actions, update);
		}
		state = dispatchTuiKey(state, { name: "s", ctrl: true }, "/tmp/ws", actions, update);
		await Bun.sleep(10);
		expect(calls.some(c => c.startsWith("send:note:Hi:"))).toBe(true);

		// Switch to members pane ('4'), start interactive key exchange ('k'), confirm ('enter')
		state = dispatchTuiKey(state, { name: "4", char: "4" }, "/tmp/ws", actions, update);
		expect(state.activePane).toBe("members");
		state = dispatchTuiKey(state, { name: "k", char: "k" }, "/tmp/ws", actions, update);
		await Bun.sleep(10);
		expect(state.modal?.kind).toBe("exchange");
		state = dispatchTuiKey(state, { name: "enter" }, "/tmp/ws", actions, update);
		await Bun.sleep(10);
		expect(calls).toContain("trustBegin:urn:onlyne:agent:peer-b");
		expect(calls).toContain("trustConfirm:urn:onlyne:agent:peer-b:fp-peer-b-9876543210");

		// Bootstrap wizard validation & save
		state = dispatchTuiKey(state, { name: "i", char: "i" }, "/tmp/ws", actions, update);
		expect(state.bootstrap).toBeDefined();
		const invalid = validateBootstrapValues({
			...state.bootstrap!.values,
			policy: "segment",
			allow: "192.168.1.1/99",
		}, "en");
		expect(invalid.ok).toBe(false);

		state = dispatchTuiKey(state, { name: "s", ctrl: true }, "/tmp/ws", actions, update);
		await Bun.sleep(10);
		expect(calls.some(c => c.startsWith("saveConfig:"))).toBe(true);
		expect(state.bootstrap).toBeUndefined();
	});
});
