import { defineTool, type ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { randomUUID } from "node:crypto";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { Type } from "typebox";
import { broadcast, connectDaemon, consumeEvent, loopback, markConsumed, sendWithRetry, shutdownDaemon, stopProcess, subscribe, swarmReady, swarmRecycled } from "./onlyne.js";
import type { SendTarget } from "./onlyne.js";
import { inboundModeFor, loadConfig, saveConfig } from "./config.js";
import { findWorkspace, type Workspace } from "./workspace.js";
import { envTaskId, parseSwarmCtl, parseSwarmHeader, readSwarmEnabled, readSwarmModel, terminalHandle, treePathForWorkspace } from "./swarm.js";
import { SwarmSlot } from "./swarm-slot.js";
const swarmSlot = new SwarmSlot();

interface Inbound { channelId: string; conversationId: string; messageId?: string; text: string; replied: boolean; noReply: boolean; reminders: number; fallbackText?: string }
/** Swarm task slot: one session carries exactly one hop. No waiting, no callbacks. */
interface SwarmTask { taskId: string; from: string; transferSendTo: string; attempt: number }

interface State {
	cwd: string; workspace: Workspace | null; watching: boolean; owner: "external" | "extension" | "stopped";
	child?: any; socket?: any; reconnectTimer?: ReturnType<typeof setTimeout>; reminderTimer?: ReturnType<typeof setTimeout>;
	currentInbound?: Inbound; lastValidOutput?: string;
	/** Swarm mode: bound to this workspace .onlyne/config.toml [swarm] enabled. */
	swarm: boolean;
	/** Active swarm task (single atomic slot, one hop). */
	swarmTask?: SwarmTask;
}
const state: State = { cwd: process.cwd(), workspace: null, watching: false, owner: "stopped", swarm: false };
let cachedCtx: any = undefined;
const textResult = (text: string, details?: unknown) => ({ content: [{ type: "text" as const, text }], details });
const currentConfig = () => loadConfig(state.cwd);
function refreshSwarmFlag() { state.swarm = state.workspace ? readSwarmEnabled(state.workspace.onlyneDir) : false; }
function inboundText(data: any) { const msg = data?.data?.data ?? data?.data ?? data; const channelId = msg.channel_id ?? msg.channelId; const conversationId = msg.conversation_id ?? msg.conversationId; const messageId = msg.message_id ?? msg.messageId; const text = msg.text ?? msg.content ?? msg.body; return channelId && conversationId && typeof text === "string" ? { channelId, conversationId, messageId, text } : null; }
function consumeIfNotified(inbound: { messageId?: string }) { if (state.workspace && inbound.messageId) void markConsumed(state.workspace.socketPath, inbound.messageId).catch(() => {}); }
function clearReminder() { if (state.reminderTimer) clearTimeout(state.reminderTimer); state.reminderTimer = undefined; }
function needsReply(inbound = state.currentInbound) { return !!inbound && !inbound.replied && !inbound.noReply && !!state.workspace; }
function scheduleReminder(pi: ExtensionAPI, delayMs = 30_000) {
	clearReminder();
	const inbound = state.currentInbound;
	if (!inbound || !needsReply(inbound)) return;
	state.reminderTimer = setTimeout(() => {
		state.reminderTimer = undefined;
		if (!needsReply(inbound) || state.currentInbound !== inbound) return;
		const cfg = currentConfig();
		if (cfg.outbound.defaultReplyMode === "explicit-only") return;
		if (cfg.outbound.defaultReplyMode === "guarded-explicit" && inbound.reminders < cfg.outbound.guardedExplicit.reminders) {
			if (inbound.reminders === 0) inbound.fallbackText = state.lastValidOutput;
			inbound.reminders++;
			pi.sendUserMessage(`Onlyne reminder ${inbound.reminders}/${cfg.outbound.guardedExplicit.reminders}: reply to ${inbound.channelId}/${inbound.conversationId} with onlyne_reply, or call onlyne_mark_no_reply.`, { deliverAs: "followUp" });
			return;
		}
		void reply(inbound.fallbackText || state.lastValidOutput || cfg.outbound.guardedExplicit.noOutputFallbackText).catch(() => {});
	}, delayMs);
}
function scheduleReconnect(pi: ExtensionAPI) {
	if (state.reconnectTimer || !state.watching || !state.workspace) return;
	state.reconnectTimer = setTimeout(async () => {
		state.reconnectTimer = undefined;
		if (!state.watching) return;
		try { await startWatch(pi); }
		catch { scheduleReconnect(pi); }
	}, 1000);
}

/** Whether a history task already has an out (done) in this workspace. */
async function historyTaskDone(taskId: string): Promise<boolean> {
	try {
		const { request } = await import("./onlyne.js");
		if (!state.workspace) return false;
		const res = await request(state.workspace.socketPath, { id: "hist", op: "fetch_channel_history", channel_id: "loopback", limit: 30 } as any);
		const items = res?.data ?? res ?? [];
		if (!Array.isArray(items)) return false;
		const outs = new Set<string>();
		for (const m of items) {
			const text = m?.text ?? "";
			if (typeof text !== "string" || !text.startsWith("---swarm")) continue;
			if (text.startsWith("---swarm-ctl")) continue;
			if (m?.direction === "outbound") {
				const p = parseSwarmHeader(text);
				if (p) outs.add(p.header.task_id);
			}
		}
		if (outs.has(taskId)) return true;
		// Stale-claim guard: an inbound older than the newest outbound batch
		// belongs to a previous hop generation. Only the scheduler-assigned
		// env task bypasses this check.
		return false;
	} catch { return false; }
}

/** Replay the newest unclaimed swarm task from loopback history. Idempotent.
 * Prefers the scheduler-assigned env task (ONLYNE_SWARM_TASK); never claims
 * an already-done task unless it is the env task (env wins, scheduler owns
 * the truth about what this terminal must run). */
async function catchUpSwarmHistory(pi: ExtensionAPI, ctx?: any) {
	if (swarmSlot.task().taskId) return;
	try {
		const { request } = await import("./onlyne.js");
		if (!state.workspace) return;
		const env = envTaskId();
		const res = await request(state.workspace.socketPath, { id: "hist", op: "fetch_channel_history", channel_id: "loopback", limit: 30 } as any);
		const items = res?.data ?? res ?? [];
		if (!Array.isArray(items)) return;
		const inbounds: string[] = [];
		const outs = new Set<string>();
		for (const m of items) {
			const text = m?.text ?? m?.content ?? "";
			if (typeof text !== "string" || !text.startsWith("---swarm")) continue;
			if (text.startsWith("---swarm-ctl")) continue;
			const p = parseSwarmHeader(text);
			if (!p) continue;
			if (m?.direction === "outbound") outs.add(p.header.task_id);
			else inbounds.push(text);
		}
		// Env task first: exact task_id match on inbound, even if an out exists
		// (out may belong to a previous generation; scheduler owns the truth).
		if (env) {
			for (const text of inbounds) {
				const p = parseSwarmHeader(text)!;
				if (p.header.task_id === env) {
					if (handleSwarmInbound(pi, text, undefined, ctx)) return;
				}
			}
		}
		// Fallback: newest inbound with no out yet.
		for (let i = inbounds.length - 1; i >= 0; i--) {
			const p = parseSwarmHeader(inbounds[i]!)!;
			if (outs.has(p.header.task_id)) continue;
			if (p.header.task_id === env) continue; // already tried above
			if (handleSwarmInbound(pi, inbounds[i]!)) return;
		}
	} catch { /* best effort; live events still arrive */ }
}

/** Pin the tab title for a claimed hop. Pi rewrites the create-time title on
 * boot and on idle transitions, and the scheduler-side rename races those
 * writes. Last writer wins, so the session re-asserts its own title on claim
 * and again on agent_end (idle), which is pi's other rewrite point. */
function pinSwarmTitle(ctx: any, taskId: string) {
	try {
		ctx?.ui?.setTitle?.(`swarm:${swarmTreePath()}:${taskId.slice(0, 8)}`);
	} catch { /* title is cosmetic */ }
}

/** Swarm-mode inbound path: claim one hop, inject via followUp, never wait. */
function handleSwarmInbound(pi: ExtensionAPI, text: string, eventSeq?: number, ctx?: any) {
	const parsed = parseSwarmHeader(text);
	if (!parsed) return false;
	if (state.socket && eventSeq !== undefined) void consumeEvent(state.socket, eventSeq).catch(() => {});
	// Slot transitions live in SwarmSlot (unit-tested); here we only mirror
	// the claimed task into session state. A claimed session never accepts
	// another task; downstream work spawns new tasks via swarm_send.
	const outcome = swarmSlot.handle(pi, text, envTaskId() || undefined, state.workspace?.root);
	if (outcome === "claimed" || outcome === "yielded") {
		const cur = swarmSlot.task();
		state.swarmTask = { taskId: cur.taskId!, from: cur.from, transferSendTo: cur.transferSendTo, attempt: cur.attempt };
		if (ctx) void pinSwarmTitle(ctx, cur.taskId!);
	}
	return outcome === "claimed" || outcome === "yielded";
}

async function startWatch(pi: ExtensionAPI) {
	state.workspace = findWorkspace(state.cwd); if (!state.workspace) throw new Error("current workspace has no .onlyne configuration");
	refreshSwarmFlag();
	if (state.reconnectTimer) clearTimeout(state.reconnectTimer); state.reconnectTimer = undefined;
	state.socket?.destroy(); state.socket = undefined;
	const conn = await connectDaemon(state.workspace); state.owner = conn.owner; state.child = conn.process;
	if (state.swarm) {
		// Swarm mode: the scheduler owns in/out. Subscribe without auto-handling generic
		// traffic; only swarm headers enter the session, via the followUp task queue.
		// Report readiness so the scheduler can match a pending task (fork+exec: any
		// clean ready session on this workspace path may take it).
		const ws = state.workspace;
		// Priority 1 (above default 0): the scheduler consumes at MAX, then we
		// take the event at tier 1 so it also reaches the session. A plain
		// tier-0 subscription would starve whenever the scheduler consumes.
		const socket = subscribe(ws.socketPath, (line) => {
			if (!line?.event) return;
			if (line.type !== "inbound_message") return;
			const inbound = inboundText(line);
			if (!inbound || inbound.channelId !== "loopback") return;
			// Catch up on history first: a fresh watch may subscribe after the
			// task was already delivered (scheduler writes in before the
			// session finishes starting). History replay is idempotent via
			// the slot guard + scheduler task_id dedup.
			if (parseSwarmCtl(inbound.text)) {
				const ctl = parseSwarmCtl(inbound.text)!;
				void handleSwarmRecycle(ctl.task_id, ctl.reason);
				return;
			}
			void catchUpSwarmHistory(pi);
			handleSwarmInbound(pi, inbound.text, line.event_seq);
		}, () => { if (state.socket === socket) scheduleReconnect(pi); }, { priority: 1 });
		state.socket = socket; state.watching = true;
		const task = envTaskId();
		try { await swarmReady(ws.socketPath, ws.root, terminalHandle()); } catch { /* scheduler may read env fallback */ }
		// The task may already sit in history (scheduler wrote in before this
		// session finished starting). Claim it now instead of waiting for a
		// live event that already fired.
		await catchUpSwarmHistory(pi);
		return `swarm watching ${ws.root} (${state.owner})${task ? ` task=${task}` : ""}`;
	}
	const socket = subscribe(state.workspace.socketPath, (line) => { if (!line?.event || line.type !== "inbound_message") return; const inbound = inboundText(line); if (!inbound) return; const mode = inboundModeFor(currentConfig(), inbound.channelId, inbound.conversationId); if (mode === "muted") return; if (inbound.channelId === "loopback") { if (mode === "auto-handle") pi.sendUserMessage(`Onlyne loopback activation${inbound.conversationId ? ` (${inbound.conversationId})` : ""}:\n\n${inbound.text}`, { deliverAs: "followUp" }); consumeIfNotified(inbound); return; } if (inbound.text.trim() === "/handshake") { consumeIfNotified(inbound); return; } clearReminder(); state.currentInbound = { ...inbound, replied: false, noReply: false, reminders: 0 }; if (mode === "auto-handle") { pi.sendUserMessage(`Onlyne inbound message from ${inbound.channelId}/${inbound.conversationId}:\n\n${inbound.text}\n\nReply with onlyne_reply, or call onlyne_mark_no_reply if no reply is needed.`, { deliverAs: "followUp" }); consumeIfNotified(inbound); } }, () => { if (state.socket === socket) scheduleReconnect(pi); });
	state.socket = socket; state.watching = true; return `watching ${state.workspace.root} (${state.owner})`;
}
function stopWatch() { if (state.reconnectTimer) clearTimeout(state.reconnectTimer); state.reconnectTimer = undefined; clearReminder(); state.socket?.destroy(); state.socket = undefined; stopProcess(state.child); state.child = undefined; state.watching = false; state.owner = "stopped"; state.swarmTask = undefined; swarmSlot.clear(); return "watch stopped"; }
async function startDaemon() { state.workspace = findWorkspace(state.cwd); if (!state.workspace) throw new Error("current workspace has no .onlyne configuration"); refreshSwarmFlag(); const conn = await connectDaemon(state.workspace, true); state.owner = conn.owner; state.child = conn.process; return `daemon ${state.owner === "extension" ? "started" : "already running"} for ${state.workspace.root}`; }
async function stopDaemon() { if (!state.workspace) state.workspace = findWorkspace(state.cwd); if (!state.workspace) throw new Error("current workspace has no .onlyne configuration"); clearReminder(); state.socket?.destroy(); state.socket = undefined; await shutdownDaemon(state.workspace, state.child); state.child = undefined; state.watching = false; state.owner = "stopped"; return `daemon stopped for ${state.workspace.root}`; }
async function restartDaemon() { await stopDaemon().catch(() => {}); return startDaemon(); }
async function reply(text: string) { if (!state.workspace) throw new Error("onlyne workspace not found"); const inbound = state.currentInbound; if (!inbound) throw new Error("no active inbound message"); const res = await sendWithRetry(state.workspace.socketPath, { channelId: inbound.channelId }, text, currentConfig().outbound.retry.attempts); if (res.ok) { inbound.replied = true; clearReminder(); } return res; }
/** Resolve the current workspace tree path: segment after /.ws/, root is ".". */
function swarmTreePath(): string {
	if (!state.workspace) throw new Error("onlyne workspace not found");
	return treePathForWorkspace(state.workspace.root);
}

/** swarm_complete: hand over. Writes the out message carrying this hop's header (done signal). */
async function swarmComplete(text: string) {
	if (!state.workspace) throw new Error("onlyne workspace not found");
	const task = state.swarmTask;
	if (!task) throw new Error("no active swarm task");
	const { renderSwarmHeader } = await import("./swarm.js");
	const wire = renderSwarmHeader({ task_id: task.taskId, from: swarmTreePath(), transfer_send_to: task.transferSendTo, attempt: task.attempt }, "", text);
	const res = await sendWithRetry(state.workspace.socketPath, { channelId: "loopback" }, wire, currentConfig().outbound.retry.attempts);
	if (res.ok) { state.swarmTask = undefined; swarmSlot.clear(); }
	return { ...res, taskId: task.taskId };
}

/** swarm_quit: speak, then die by our own hand. Sends the exit notice
 * (same op the recycle path uses, reason quit:<reason>) so the scheduler
 * records failed immediately instead of leaving the task pinned running;
 * then releases the watch and exits this process. The scheduler only ever
 * closes the tab. */
async function swarmQuit(reason?: string) {
	const task = state.swarmTask;
	const taskId = task?.taskId ?? "";
	const why = `quit:${reason ?? ""}`;
	try {
		if (state.workspace && taskId) {
			await swarmRecycled(state.workspace.socketPath, taskId, terminalHandle(), why);
		}
	} catch { /* daemon may be gone; exit anyway */ }
	clearReminder();
	try { stopWatch(); } catch { /* already stopped */ }
	state.swarmTask = undefined;
	swarmSlot.clear();
	setTimeout(() => process.exit(0), 300);
	return { quit: true, taskId, reason: reason ?? "" };
}

/** Handle a downlink recycle signal: accept, ack, release, self-terminate.
 * No out is written (the scheduler already recorded the terminal state). */
async function handleSwarmRecycle(ctlTaskId: string, reason: string) {
	const claimed = swarmSlot.task().taskId;
	const taskId = ctlTaskId === "*" ? (claimed ?? "") : ctlTaskId;
	if (claimed && ctlTaskId !== "*" && claimed !== ctlTaskId) return false;
	const why = `recycle:${reason}`;
	try {
		if (state.workspace && taskId) {
			await swarmRecycled(state.workspace.socketPath, taskId, terminalHandle(), why);
		}
	} catch { /* daemon may be gone; exit anyway */ }
	clearReminder();
	try { stopWatch(); } catch { /* already stopped */ }
	state.swarmTask = undefined;
	swarmSlot.clear();
	setTimeout(() => process.exit(0), 300);
	return true;
}

/** swarm_send: spawn downstream. New UUID, transfer_send_to = current task, fire-and-forget. */
async function swarmSend(to: string, text: string) {
	if (!state.workspace) throw new Error("onlyne workspace not found");
	const task = state.swarmTask;
	if (!task) throw new Error("no active swarm task");
	const { renderSwarmHeader } = await import("./swarm.js");
	// Resolve the target workspace dir: root "." or a tree path under the swarm tree.
	// The scheduler owns the tree; here we only verify the send-side symlink exists
	// (missing target = dangling link = error, never a blind FIFO write).
	const wsRoot = state.workspace.root;
	const linkPath = to === "." || to === "_root"
		? join(wsRoot, "onlyne_in", "_root")
		: join(wsRoot, "onlyne_in", ...to.split("/").filter(Boolean));
	const resolved = resolve(wsRoot);
	const linkDir = resolve(linkPath);
	if (!linkDir.startsWith(resolved)) throw new Error(`invalid swarm_send target: ${to}`);
	if (!existsSync(linkPath)) throw new Error(`unknown swarm_send target (missing onlyne_in link): ${to}`);
	const childId = randomUUID();
	const wire = renderSwarmHeader({ task_id: childId, from: swarmTreePath(), transfer_send_to: task.taskId, attempt: 1 }, "", text);
	try {
		writeFileSync(linkPath, wire);
	} catch (e) {
		throw new Error(`swarm_send failed for ${to}: ${e instanceof Error ? e.message : String(e)}`);
	}
	swarmSlot.noteSpawned(childId);
	return { childId, to };
}

/** Swarm mode: apply the workspace snapshot model triple to this session. */
async function applySwarmModel(pi: ExtensionAPI, ctx: any) {
	if (!state.swarm || !state.workspace) return;
	const m = readSwarmModel(state.workspace.onlyneDir);
	if (!m) return;
	if (ctx?.model && m.provider && ctx.model.provider === m.provider && m.model && ctx.model.id === m.model && (!m.effort || m.effort === ctx.thinkingLevel)) return;
	try {
		if (m.provider && m.model) {
			const found = ctx.modelRegistry?.find?.(m.provider, m.model);
			if (found) {
				const ok = await pi.setModel(found);
				if (!ok) { ctx.ui?.notify(`swarm model ${m.provider}/${m.model}: no auth`, "warning"); return; }
			}
			else ctx.ui?.notify(`swarm model ${m.provider}/${m.model} not in registry; keeping default`, "warning");
		}
		const eff = m.effort;
		if (eff && ["off", "minimal", "low", "medium", "high", "xhigh", "max"].includes(eff)) pi.setThinkingLevel(eff as any);
		ctx.ui?.setStatus?.("onlyne-model", `model:${m.provider ?? ""}/${m.model ?? ""}${m.effort ? ":" + m.effort : ""}`);
	} catch (e) { ctx.ui?.notify(`swarm model apply failed: ${e}`, "warning"); }
}

export default function onlyne(pi: ExtensionAPI) {
	pi.on("session_start", async (_event, ctx) => { cachedCtx = ctx; const resumeWatch = state.watching; if (state.owner === "extension") await stopDaemon().catch(() => {}); else stopWatch(); state.cwd = ctx.cwd; state.workspace = findWorkspace(ctx.cwd); state.currentInbound = undefined; state.lastValidOutput = undefined; state.swarmTask = undefined; swarmSlot.clear(); refreshSwarmFlag(); applyToolSurface(pi); void applySwarmModel(pi, ctx); ctx.ui.setStatus("onlyne", state.workspace ? (state.swarm ? "onlyne: swarm" : "onlyne: ready") : "onlyne: no .onlyne"); if ((currentConfig().watch.autoStart || resumeWatch) && state.workspace) { try { ctx.ui.notify(await startWatch(pi), "info"); } catch (e) { ctx.ui.notify(String(e), "warning"); } } });
	pi.on("session_shutdown", async () => { if (state.owner === "extension") await stopDaemon().catch(() => {}); else stopWatch(); });
	for (const sig of ["SIGINT", "SIGTERM", "SIGHUP"] as const) process.once(sig, () => stopWatch());
	pi.on("message_end", async (event) => { const text = typeof (event as any).content === "string" ? (event as any).content.trim() : ""; if (text && !text.startsWith("{") && !text.startsWith("[onlyne-internal]")) state.lastValidOutput = text; });
	pi.on("agent_start", async () => clearReminder());
	pi.on("agent_end", async () => { if (state.swarm) { const cur = swarmSlot.task(); if (cur.taskId && cachedCtx) void pinSwarmTitle(cachedCtx, cur.taskId); scheduleSwarmExitReminder(pi); } else scheduleReminder(pi); });
	pi.registerCommand("onlyne", {
		description: "Onlyne watch/status/config commands",
		getArgumentCompletions: (prefix: string) => {
			const commands = ["status", "watch on", "watch off", "daemon start", "daemon stop", "daemon restart", "config auto-start", "swarm on", "swarm off", "swarm status"];
			const p = prefix.trimStart();
			const filtered = commands.filter((c) => c.startsWith(p));
			return filtered.length ? filtered.map((value) => ({ value, label: value })) : null;
		},
		handler: async (argLine: string, ctx: any) => {
			const [cmd, sub] = argLine.trim().split(/\s+/);
			try {
				if (cmd === "watch" && sub === "on") ctx.ui.notify(await startWatch(pi), "info");
				else if (cmd === "watch" && sub === "off") ctx.ui.notify(stopWatch(), "info");
				else if (cmd === "daemon" && sub === "start") ctx.ui.notify(await startDaemon(), "info");
				else if (cmd === "daemon" && sub === "stop") ctx.ui.notify(await stopDaemon(), "info");
				else if (cmd === "daemon" && sub === "restart") ctx.ui.notify(await restartDaemon(), "info");
				else if (cmd === "status") ctx.ui.notify(`onlyne ${state.watching ? "watching" : "stopped"}; owner=${state.owner}; swarm=${state.swarm ? "on" : "off"}; workspace=${state.workspace?.root ?? "none"}`, "info");
				else if (cmd === "swarm" && sub === "status") { const cur = swarmSlot.task(); ctx.ui.notify(`swarm=${state.swarm ? "on" : "off"}; task=${cur.taskId ?? "none"}; spawned=${cur.sentChildIds.length}`, "info"); }
				else if (cmd === "swarm" && (sub === "on" || sub === "off")) { ctx.ui.notify(await setSwarm(pi, sub === "on"), "info"); }
				else if (cmd === "config" && sub === "auto-start") { const cfg = currentConfig(); cfg.watch.autoStart = !cfg.watch.autoStart; saveConfig(state.cwd, cfg); ctx.ui.notify(`autoStart=${cfg.watch.autoStart}`, "info"); }
				else ctx.ui.notify("usage: /onlyne status | watch on|off | daemon start|stop|restart | swarm on|off|status | config auto-start", "info");
			} catch (e) { ctx.ui.notify(e instanceof Error ? e.message : String(e), "error"); }
		},
	});
	registerNormalTools(pi);
	registerSwarmTools(pi);
	applyToolSurface(pi);
}

const NORMAL_TOOLS = ["onlyne_daemon_start", "onlyne_daemon_stop", "onlyne_daemon_restart", "onlyne_reply", "onlyne_send", "onlyne_broadcast", "onlyne_loopback", "onlyne_mark_no_reply"];
const SWARM_TOOLS = ["onlyne_daemon_start", "onlyne_daemon_stop", "onlyne_daemon_restart", "swarm_complete", "swarm_quit", "swarm_send", "swarm_status"];

/** Tool surface follows [swarm] at session start: one mode, one toolset. */
function applyToolSurface(pi: ExtensionAPI) {
	try {
		const active: Set<string> = new Set(typeof (pi as any).getActiveTools === "function" ? (pi as any).getActiveTools() : []);
		const all: string[] = typeof (pi as any).getAllTools === "function" ? (pi as any).getAllTools().map((t: any) => t.name) : [];
		const keep = state.swarm ? SWARM_TOOLS : NORMAL_TOOLS;
		const next: string[] = [...active].filter((n) => all.includes(n) && !(NORMAL_TOOLS.includes(n) || SWARM_TOOLS.includes(n)));
		for (const n of keep) if (all.includes(n as string) && !next.includes(n as string)) next.push(n);
		if (typeof (pi as any).setActiveTools === "function") (pi as any).setActiveTools(next);
	} catch { /* older pi without tool-surface API: both sets stay registered */ }
}

function registerNormalTools(pi: ExtensionAPI) {
	pi.registerTool(defineTool({ name: "onlyne_daemon_start", label: "Onlyne daemon start", description: "Start or connect to the current workspace-local Onlyne daemon managed by pi-onlyne.", parameters: Type.Object({}), executionMode: "parallel", async execute() { const res = await startDaemon(); return textResult(res, { owner: state.owner, workspace: state.workspace?.root }); } }));
	pi.registerTool(defineTool({ name: "onlyne_daemon_stop", label: "Onlyne daemon stop", description: "Stop the current workspace-local Onlyne daemon when pi-onlyne manages it, without shelling out to pkill/nohup.", parameters: Type.Object({}), executionMode: "parallel", async execute() { const res = await stopDaemon(); return textResult(res, { owner: state.owner, workspace: state.workspace?.root }); } }));
	pi.registerTool(defineTool({ name: "onlyne_daemon_restart", label: "Onlyne daemon restart", description: "Restart the current workspace-local Onlyne daemon through pi-onlyne lifecycle management.", parameters: Type.Object({}), executionMode: "parallel", async execute() { const res = await restartDaemon(); return textResult(res, { owner: state.owner, workspace: state.workspace?.root }); } }));
	pi.registerTool(defineTool({ name: "onlyne_reply", label: "Onlyne reply", description: "Reply with plain text to the current Onlyne inbound message.", parameters: Type.Object({ text: Type.String() }), executionMode: "parallel", async execute(_id, params) { return textResult(JSON.stringify(await reply(params.text))); } }));
	pi.registerTool(defineTool({ name: "onlyne_send", label: "Onlyne send", description: "Send Markdown to the channel's configured Onlyne conversation. Set rawText=true only for literal plain text.", parameters: Type.Object({ channelId: Type.String(), text: Type.String(), rawText: Type.Optional(Type.Boolean()) }), executionMode: "parallel", async execute(_id, params) { if (!state.workspace) throw new Error("onlyne workspace not found"); const res = await sendWithRetry(state.workspace.socketPath, params, params.text, currentConfig().outbound.retry.attempts, params.rawText ?? false); return textResult(JSON.stringify(res), res); } }));
	pi.registerTool(defineTool({ name: "onlyne_broadcast", label: "Onlyne broadcast", description: "Send Markdown to many configured Onlyne channels concurrently. Set rawText=true only for literal plain text.", parameters: Type.Object({ targets: Type.Array(Type.Object({ channelId: Type.String() })), text: Type.String(), rawText: Type.Optional(Type.Boolean()) }), executionMode: "parallel", async execute(_id, params) { if (!state.workspace) throw new Error("onlyne workspace not found"); const cfg = currentConfig(); const results = await broadcast(state.workspace.socketPath, params.targets as SendTarget[], params.text, cfg.outbound.retry.attempts, cfg.outbound.retry.concurrency, params.rawText ?? false); return textResult(JSON.stringify({ ok: results.every((r) => r.ok), results }), results); } }));
	pi.registerTool(defineTool({ name: "onlyne_loopback", label: "Onlyne loopback", description: "Inject a local loopback activation message so scripts can wake the current Pi session. Set rawText=false for Markdown. FIFO alternative: write to .onlyne/channels/loopback/in.", parameters: Type.Object({ text: Type.String(), rawText: Type.Optional(Type.Boolean()) }), executionMode: "parallel", async execute(_id, params) { if (!state.workspace) throw new Error("onlyne workspace not found"); const res = await loopback(state.workspace.socketPath, params.text, params.rawText ?? true); return textResult(JSON.stringify(res), res); } }));
	pi.registerTool(defineTool({ name: "onlyne_mark_no_reply", label: "Onlyne no reply", description: "Mark the current Onlyne inbound message as intentionally not replied.", parameters: Type.Object({ reason: Type.Optional(Type.String()) }), executionMode: "parallel", async execute(_id, params) { if (state.currentInbound) { state.currentInbound.noReply = true; clearReminder(); } return textResult("marked no reply", params); } }));
}

function registerSwarmTools(pi: ExtensionAPI) {
	pi.registerTool(defineTool({ name: "swarm_complete", label: "Swarm complete", description: "Hand over this swarm hop: write the out message carrying this task header (done signal). The session becomes recyclable. Use once per swarm task.", parameters: Type.Object({ text: Type.String() }), executionMode: "parallel", async execute(_id, params) { return textResult(JSON.stringify(await swarmComplete(params.text))); } }));
	pi.registerTool(defineTool({ name: "swarm_quit", label: "Swarm quit", description: "Quit this swarm hop silently: no out is written and the scheduler records failed. Use when the task premise does not hold or there is nothing to do.", parameters: Type.Object({ reason: Type.Optional(Type.String()) }), executionMode: "parallel", async execute(_id, params) { return textResult(JSON.stringify(await swarmQuit(params.reason))); } }));
	pi.registerTool(defineTool({ name: "swarm_send", label: "Swarm send", description: "Spawn a downstream swarm task: write a new task to onlyne_in/<to>/ with transfer_send_to set to the current task. Fire-and-forget; returns the child id without waiting. Errors on missing targets.", parameters: Type.Object({ to: Type.String(), text: Type.String() }), executionMode: "parallel", async execute(_id, params) { return textResult(JSON.stringify(await swarmSend(params.to, params.text))); } }));
	pi.registerTool(defineTool({ name: "swarm_status", label: "Swarm status", description: "Read-only swarm state: current task id, from, workspace path, and spawned child ids.", parameters: Type.Object({}), executionMode: "parallel", async execute() { const cur = swarmSlot.task(); return textResult(JSON.stringify({ taskId: cur.taskId ?? null, from: cur.from, transferSendTo: cur.transferSendTo, workspace: state.workspace?.root ?? null, spawned: cur.sentChildIds })); } }));
}

/** Swarm exit guard: at agent end, an unfinished hop gets one reminder, then auto-quit. */
function scheduleSwarmExitReminder(pi: ExtensionAPI, delayMs = 30_000) {
	clearReminder();
	if (!state.swarmTask) return;
	state.reminderTimer = setTimeout(() => {
		state.reminderTimer = undefined;
		if (!state.swarmTask) return;
		if (state.swarmTask) {
{ const p = pi as any; if (typeof p.sendMessage === "function") p.sendMessage({ customType: "onlyne-swarm-reminder", content: `Swarm hop ${state.swarmTask.taskId} has no exit yet. Call swarm_complete with the handover summary, or swarm_quit when there is nothing to do.`, display: true }, { triggerTurn: true, deliverAs: "followUp" }); else pi.sendUserMessage(`Swarm hop ${state.swarmTask.taskId} has no exit yet. Call swarm_complete with the handover summary, or swarm_quit when there is nothing to do.`, { deliverAs: "followUp" }); }
			state.reminderTimer = setTimeout(() => {
				state.reminderTimer = undefined;
				if (state.swarmTask) void swarmQuit("auto-quit: no explicit exit").catch(() => {});
			}, delayMs);
		}
	}, delayMs);
}

/** Toggle swarm mode: persists to .onlyne/config.toml [swarm] enabled, restarts watch. */
async function setSwarm(pi: ExtensionAPI, enabled: boolean) {
	if (!state.workspace) throw new Error("onlyne workspace not found");
	const cfgPath = join(state.workspace.onlyneDir, "config.toml");
	let text = existsSync(cfgPath) ? readFileSync(cfgPath, "utf8") : "";
	if (text.includes("[swarm]")) {
		const lines = text.split("\n");
		let inSwarm = false;
		text = lines.map((line) => {
			const t = line.trim();
			if (t.startsWith("[")) { inSwarm = t === "[swarm]"; return line; }
			if (inSwarm && t.startsWith("enabled")) return `enabled = ${enabled}`;
			return line;
		}).join("\n");
	} else {
		if (text && !text.endsWith("\n")) text += "\n";
		text += `\n[swarm]\nenabled = ${enabled}\n`;
	}
	writeFileSync(cfgPath, text);
	refreshSwarmFlag();
	if (state.watching) { stopWatch(); return `${enabled ? "swarm on; " : "swarm off; "}${await startWatch(pi)}`; }
	return `swarm ${enabled ? "on" : "off"} (watch not running)`;
}
