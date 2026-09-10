import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";

/** Swarm header carried in the message body (upper-layer protocol, see PROTOCOL.md). */
export interface SwarmHeader { task_id: string; from: string; transfer_send_to: string; attempt: number }
/** `scheduler` identifies the scheduler's second delivery. Raw relay and out
 * wires omit it, so a cold-start history scan cannot claim them. */
export interface SwarmMessage { header: SwarmHeader; delivery: string; payload: string }

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** Parse a `---swarm` body header. Returns null for non-swarm messages. */
export function parseSwarmHeader(text: string): SwarmMessage | null {
	const rest = text.startsWith("---swarm\n") ? text.slice("---swarm\n".length) : null;
	if (rest === null) return null;
	let end = rest.indexOf("\n---\n");
	let payload: string;
	let headRaw: string;
	if (end >= 0) {
		headRaw = rest.slice(0, end);
		payload = rest.slice(end + "\n---\n".length);
	} else if (rest.endsWith("\n---")) {
		headRaw = rest.slice(0, rest.length - "\n---".length);
		payload = "";
	} else {
		const alt = rest.indexOf("\n---");
		if (alt < 0) return null;
		headRaw = rest.slice(0, alt);
		payload = rest.slice(alt + "\n---".length).replace(/^\n/, "");
	}
	let task_id: string | undefined;
	let from = ".";
	let transfer_send_to = "";
	let attempt = 1;
	let delivery = "";
	let sawTransfer = false;
	for (const line of headRaw.split("\n")) {
		const t = line.trim();
		if (!t || t.startsWith("#")) continue;
		const i = t.indexOf(":");
		if (i < 0) continue;
		const k = t.slice(0, i).trim();
		const v = t.slice(i + 1).trim().replace(/^["']|["']$/g, "");
		if (k === "task_id") task_id = v;
		else if (k === "from") from = v || ".";
		else if (k === "transfer_send_to") { transfer_send_to = v; sawTransfer = true; }
		else if (k === "reply_to") return null;
		else if (k === "attempt") attempt = Number.parseInt(v, 10) || 1;
		else if (k === "delivery") delivery = v;
	}
	if (!sawTransfer) return null;
	if (!task_id || !UUID_RE.test(task_id.trim())) return null;
	return { header: { task_id: task_id.trim(), from, transfer_send_to, attempt }, delivery, payload };
}

/** Downlink control wire (scheduler -> session). Header-only marker the
 * session intercepts before the model sees anything. `task_id` is "*" for
 * "whatever this session holds". */
export interface SwarmCtl { op: string; task_id: string; reason: string }
export function parseSwarmCtl(text: string): SwarmCtl | null {
	if (!text.startsWith("---swarm-ctl\n")) return null;
	const rest = text.slice("---swarm-ctl\n".length);
	const end = rest.indexOf("\n---");
	if (end < 0) return null;
	let op: string | undefined;
	let task_id = "*";
	let reason = "";
	for (const line of rest.slice(0, end).split("\n")) {
		const t = line.trim();
		if (!t || t.startsWith("#")) continue;
		const i = t.indexOf(":");
		if (i < 0) continue;
		const k = t.slice(0, i).trim();
		const v = t.slice(i + 1).trim().replace(/^["']|["']$/g, "");
		if (k === "op") op = v;
		else if (k === "task_id") task_id = v || "*";
		else if (k === "reason") reason = v;
	}
	if (op !== "recycle") return null;
	return { op, task_id, reason };
}

/** Render a swarm body header in front of a Markdown payload. */
export function renderSwarmHeader(header: SwarmHeader, role: string, payloadMarkdown: string): string {
	// Raw relay and out wires intentionally omit `delivery`. Scheduler owns the
	// second delivery step and adds `delivery: scheduler` there.
	let s = `---swarm\ntask_id: ${header.task_id}\nfrom: ${header.from}\ntransfer_send_to: ${header.transfer_send_to}\nattempt: ${header.attempt}\n---\n`;
	// Flat role text follows the delimiter once, with no duplicate echo.
	if (role) {
		s += role.endsWith("\n") ? role + "\n" : role + "\n\n";
	}
	s += payloadMarkdown;
	if (!s.endsWith("\n")) s += "\n";
	return s;
}

/** Read the `[swarm] enabled` flag from the workspace .onlyne/config.toml. */
export function readSwarmEnabled(onlyneDir: string): boolean {
	const path = join(onlyneDir, "config.toml");
	if (!existsSync(path)) return false;
	try {
		const text = readFileSync(path, "utf8");
		let inSwarm = false;
		for (const line of text.split("\n")) {
			const t = line.trim();
			if (t.startsWith("[")) { inSwarm = t === "[swarm]"; continue; }
			if (inSwarm && t.startsWith("enabled")) {
				return /=?\s*true\b/.test(t);
			}
		}
	} catch { /* unreadable -> disabled */ }
	return false;
}

/** Strip // line comments and trailing commas so jsonc parses as JSON. */
export function stripJsonc(text: string): string {
	const out: string[] = [];
	let inStr = false;
	let esc = false;
	for (const line of text.split("\n")) {
		let res = "";
		for (let i = 0; i < line.length; i++) {
			const c = line[i];
			if (inStr) {
				res += c;
				if (esc) esc = false;
				else if (c === "\\") esc = true;
				else if (c === '"') inStr = false;
				continue;
			}
			if (c === '"') { inStr = true; res += c; continue; }
			if (c === "/" && line[i + 1] === "/") break;
			res += c;
		}
		out.push(res);
	}
	return out.join("\n").replace(/,(\s*[}\]])/g, "$1");
}

/** Read the model triple from the workspace .onlyne/swarm.workspace.jsonc snapshot. */
export function readSwarmModel(onlyneDir: string): { provider?: string; model?: string; effort?: string } | null {
	const path = join(onlyneDir, "swarm.workspace.jsonc");
	if (!existsSync(path)) return null;
	try {
		const parsed = JSON.parse(stripJsonc(readFileSync(path, "utf8")));
		const m = parsed?.model;
		if (!m || typeof m !== "object") return null;
		const provider = typeof m.provider === "string" && m.provider ? m.provider : undefined;
		const model = typeof m.model === "string" && m.model ? m.model : undefined;
		const effort = typeof m.effort === "string" && m.effort ? m.effort : undefined;
		if (!provider && !model) return null;
		return { provider, model, effort };
	} catch { /* malformed -> no model override */ }
	return null;
}

/** Tree path of a workspace inside the swarm tree: the segment after the
 * nearest `/.ws/` marker; the swarm root itself is "." .
 * Pure function so quoting/edge cases are unit-tested without a live session. */
export function treePathForWorkspace(workspaceRoot: string): string {
	const i = workspaceRoot.lastIndexOf("/.ws/");
	if (i >= 0) return workspaceRoot.slice(i + "/.ws/".length) || ".";
	return ".";
}

/** Terminal handle for swarm_ready correlation (injected by the scheduler). */
export function terminalHandle(): string {
	return process.env.ORCA_TERMINAL_HANDLE || process.env.ONLYNE_TERMINAL_HANDLE || "";
}

/** Task id injected by the scheduler via env (fallback before swarm_ready handshake). */
export function envTaskId(): string {
	return process.env.ONLYNE_SWARM_TASK || "";
}
