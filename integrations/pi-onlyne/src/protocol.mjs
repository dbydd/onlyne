// Wire vocabulary of `crates/onlyne-adapter/PROTOCOL.md`, expressed as plain
// JSON builders. Nothing here opens a socket or touches pi: every function is a
// pure translation from plugin state to one frame body, which is what makes the
// protocol testable against the Rust-side fixtures.
//
// Frame envelope, plugin -> host (WireMessage in onlyne-adapter/src/lib.rs):
//   request  {"id":N,"op":"...","args":{...}}
//   response {"reply_to":N,"ok":true,"data":{...}} | {"reply_to":N,"ok":false,"error":{code,message,field}}
// Host -> plugin notifications carry `op`/`args` and no `id`.

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

/** Protocol revision this plugin speaks; `hello.args.protocol`. */
export const PROTOCOL_VERSION = 1;
/** Plugin name on the wire; `hello.args.plugin`. */
export const PLUGIN_NAME = "pi-onlyne";
/** Mount class; the host matches the payload's field set against its variants. */
export const MOUNT_KIND = "agent";
/** Ledger summary ceiling, the same 200 characters `docs/v1-PLAN.md` fixes. */
export const MAX_HEAD_CHARS = 200;

/**
 * Base of the plugin's own report sequence.
 *
 * The host stamps its dispatch events (created, resource attach, ready) into the
 * same `(generation, seq)` watermark the plugin reports against, and the reducer
 * silently drops a report at or below that watermark. The host's own events live
 * in the low single digits, so the plugin's stream starts clear of them; without
 * the offset a turn-state observation would be dropped and never reach the
 * projection.
 */
export const SEQ_BASE = 1000;

/** Heartbeat cadence while a task is active (`heartbeat_timeout_ms` is 30s). */
export const DEFAULT_HEARTBEAT_MS = 10_000;

/** Outcomes the wire accepts for one completion. */
export const OUTCOMES = ["done", "failed", "cancelled"];

/** Mime types `ImagePart` accepts, in the core's stable order. */
export const IMAGE_MIMES = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/** Decoded ceiling for one inline image, byte-for-byte `IMAGE_DATA_MAX_BYTES`. */
export const IMAGE_DATA_MAX_BYTES = 2 * 1024 * 1024;

/**
 * The plugin's own semver, read from the package next to this file so the
 * vendored copy advertises what its `package.json` says.
 * @returns {string}
 */
export function readPluginVersion() {
  try {
    const here = dirname(fileURLToPath(import.meta.url));
    const pkg = JSON.parse(readFileSync(join(here, "..", "package.json"), "utf8"));
    return typeof pkg.version === "string" ? pkg.version : "0.0.0";
  } catch {
    return "0.0.0";
  }
}

/** One uuid v4, the shape every envelope id and task id takes. */
export function uuid() {
  return globalThis.crypto.randomUUID();
}

/**
 * `hello.args`, the first frame on the connection. The mount is flat and
 * untagged with `kind` as its sibling, exactly as PROTOCOL.md requires.
 * @param {{ role: string, session?: string | null, taskId?: string | null, pid?: number, capabilities: string[] }} options
 */
export function helloArgs({ role, session = null, taskId = null, pid, capabilities }) {
  const mount = { role };
  if (session) mount.session = session;
  if (taskId) mount.task_id = taskId;
  if (typeof pid === "number") mount.pid = pid;
  return {
    protocol: PROTOCOL_VERSION,
    plugin: PLUGIN_NAME,
    version: readPluginVersion(),
    kind: MOUNT_KIND,
    capabilities,
    mount,
  };
}

/**
 * The `welcome` args the host answers `hello` with. The host sends it inside a
 * response body as `{op:"welcome",args:{...}}`; this accepts either the whole
 * body or the already-unwrapped args object.
 * @param {any} data
 */
export function welcomeFrom(data) {
  const args = data && typeof data === "object" && data.args && data.op === "welcome" ? data.args : data;
  if (!args || typeof args !== "object" || typeof args.role !== "string") return null;
  return {
    protocol: args.protocol ?? PROTOCOL_VERSION,
    role: args.role,
    sessionId: args.session_id ?? null,
    generation: typeof args.generation === "number" ? args.generation : 1,
    prose: typeof args.prose === "string" ? args.prose : "",
    server: args.server ?? null,
    hostCapabilities: Array.isArray(args.host_capabilities) ? args.host_capabilities : [],
  };
}

/** `report.ready` — the ready barrier the payload waits behind. */
export function readyReport({ taskId, sessionId, generation, seq }) {
  return { kind: "ready", data: { task_id: taskId, session_id: sessionId, generation, seq } };
}

/**
 * `report.heartbeat`. `observed` is a full `Observation` (`onlyne-session`'s
 * reducer type), not a loose status string: the host deserialises it and rejects
 * anything that is not a legal state tuple.
 */
export function heartbeatReport({ taskId, generation, seq, agent, host = null }) {
  return {
    kind: "heartbeat",
    data: {
      task_id: taskId,
      generation,
      seq,
      observed: observationFor(agent, { generation, seq, host }),
    },
  };
}

/** `report.complete` — the terminal fact the ledger keeps. */
export function completeReport({ taskId, outcome, head }) {
  const report = { kind: "complete", data: { task_id: taskId, outcome: normalizeOutcome(outcome) } };
  const text = headOf(head);
  if (text) report.data.head = text;
  return report;
}

/** `session_register` — bind this live process to its task and session. */
export function sessionRegisterArgs({ sessionId, taskId = null, generation, pid, title = null }) {
  const args = { session_id: sessionId, generation };
  if (typeof pid === "number") args.pid = pid;
  if (title) args.title = title;
  if (taskId) args.task_id = taskId;
  return args;
}

/** `assign_ack` — the plugin's answer to one `assign`. */
export function assignAckArgs({ taskId, accepted, reason = null }) {
  const args = { task_id: taskId, accepted };
  if (reason) args.reason = reason;
  return args;
}

/** `detach` — the plugin is leaving while its session continues. */
export function detachArgs(reason) {
  return { reason };
}

/**
 * A legal `Observation` for one agent state.
 *
 * `onlyne-session`'s `is_legal` requires `public` to be `project(...)` of the
 * other dimensions and non-zero reconcile policy, so the tuple is built rather
 * than passed through: the plugin owns the agent dimension (the host never
 * synthesises turn state), and leaves delivery at `none`/outcome `pending`,
 * which is its own truth until it reports a completion.
 *
 * `host` is where this process runs (`hostBinding`); it is attached only when
 * the environment names a pane, so a pi outside Orca reports a tuple with no
 * host field at all.
 * @param {"booting"|"ready"|"running"|"idle"|"gone"} agent
 */
export function observationFor(agent, { generation, seq, host = null }) {
  const state = agent === "booting" || agent === "gone" ? "booting" : agent;
  const observed = {
    version: { generation, seq },
    generation_live: true,
    // onlyne-session/src/reconcile.rs: DEFAULT_ISOLATE_AFTER / DEFAULT_TERMINATE_AFTER.
    isolate_after: 1,
    terminate_after: 3,
    mismatch_count: 0,
    agent: state,
    delivery: "none",
    resource: "attached",
    recovery: "none",
    outcome: "pending",
    public: state === "running" ? "working" : state === "ready" || state === "idle" ? "idle" : "created",
  };
  if (host) observed.host = host;
  return observed;
}

/**
 * The Orca pane this process was spawned in, in the shape `onlyne-session`'s
 * `HostRef` deserialises (`{"orca":{…}}`), or null outside a pane.
 *
 * An Orca pane exports `ORCA_PANE_KEY` — `<tab_id>:<leaf_id>`, beside
 * `ORCA_TAB_ID` and `ORCA_TERMINAL_HANDLE` — into the command it was started
 * with (measured 2026-09-11 on Orca 1.4.198), and this plugin is spawned with
 * the environment it inherited, so the binding is inherited rather than
 * guessed: this process is the only component that can state, from the inside,
 * which Orca pane an onlyne session is. No field is invented: the ids come from
 * the key and from the environment, and a missing handle is simply absent.
 *
 * @param {Record<string, string | undefined>} env
 */
export function hostBinding(env) {
  const named = typeof env?.ORCA_PANE_KEY === "string" ? env.ORCA_PANE_KEY.trim() : "";
  const [keyTab, keyLeaf] = named.includes(":") ? named.split(":") : [];
  const tabId = env?.ORCA_TAB_ID || keyTab || "";
  const leafId = env?.ORCA_LEAF_ID || keyLeaf || "";
  if (!tabId || !leafId) return null;
  const orca = { pane_key: `${tabId}:${leafId}`, tab_id: tabId, leaf_id: leafId };
  const handle = env?.ORCA_TERMINAL_HANDLE;
  if (typeof handle === "string" && handle) orca.handle = handle;
  return { orca };
}

/** One inline image part, from raw bytes. */
export function imagePart({ data, mime, name = null }) {
  if (!IMAGE_MIMES.includes(mime)) {
    throw new Error(`image mime ${mime} unsupported; expected one of ${IMAGE_MIMES.join(", ")}`);
  }
  if (data.length > IMAGE_DATA_MAX_BYTES) {
    throw new Error(`image exceeds ${IMAGE_DATA_MAX_BYTES} bytes`);
  }
  const part = { data_base64: Buffer.from(data).toString("base64"), mime };
  if (name) part.name = name;
  return part;
}

/** `{"role":{"role":name}}`, the role principal spelling every envelope uses. */
export function principalRole(role) {
  return { role: { role } };
}

/**
 * One outbound envelope for the `send` frame.
 *
 * `note` is free text and carries no idempotency key; `task` hands work to a
 * role and requires `op_id` plus a causality chain (`Envelope::validate`).
 * @param {{ from: string, to: string, kind?: "note" | "task", text?: string, image?: object | null, taskId?: string | null }} options
 */
export function sendEnvelope({ from, to, kind = "note", text = "", image = null, taskId = null }) {
  const body = {};
  if (text) body.text = text;
  if (image) body.image = image;
  if (!body.text && !body.image) throw new Error("body requires text or image");
  const envelope = {
    protocol: PROTOCOL_VERSION,
    id: uuid(),
    kind,
    from: principalRole(from),
    to: principalRole(to),
    body,
    ts: new Date().toISOString(),
    admin: false,
  };
  if (kind === "task") {
    envelope.op_id = `o-${uuid()}`;
    envelope.causality = { task: taskId || uuid(), hop: 0, attempt: 0 };
  }
  return envelope;
}

/** A human-readable principal, for injection headers. */
export function describePrincipal(principal) {
  if (!principal || typeof principal !== "object") return "unknown";
  if (principal.role) {
    const role = principal.role.role ?? "?";
    return principal.role.session ? `role:${role}/${principal.role.session}` : `role:${role}`;
  }
  if (principal.gateway) {
    const { gateway, channel, conversation } = principal.gateway;
    return `gateway:${gateway}:${channel}${conversation ? `:${conversation}` : ""}`;
  }
  if (principal.cluster) return `cluster:${principal.cluster}`;
  return "unknown";
}

/** One line of text, trimmed and cut to the ledger ceiling. */
export function headOf(text) {
  if (typeof text !== "string") return "";
  const flat = text.replace(/\s+/g, " ").trim();
  return flat.length > MAX_HEAD_CHARS ? flat.slice(0, MAX_HEAD_CHARS) : flat;
}

/** One known outcome spelling, defaulting to `done`. */
export function normalizeOutcome(value) {
  return typeof value === "string" && OUTCOMES.includes(value) ? value : "done";
}

/**
 * The user message one `assign` becomes.
 *
 * The task text is quoted verbatim under a header naming its origin, so a
 * session transcript shows where the instruction came from; the role prose
 * (identical in `welcome` and `assign`) is folded in only when it has not
 * already been delivered.
 * @param {{ assign: any, proseIsNew: boolean, attachmentPaths?: string[] }} options
 */
export function injectionText({ assign, proseIsNew, attachmentPaths = [] }) {
  const envelope = assign.envelope ?? {};
  const taskId = assign.task_id ?? envelope.causality?.task ?? "unknown";
  const lines = [
    `[onlyne] task ${taskId} from ${describePrincipal(envelope.from)} (kind ${envelope.kind ?? "task"})`,
  ];
  const prose = typeof assign.prose === "string" ? assign.prose.trim() : "";
  if (prose && proseIsNew) {
    lines.push("", "[onlyne] role prose from the spec:", prose);
  }
  const text = typeof envelope.body?.text === "string" ? envelope.body.text.trim() : "";
  lines.push("", text || "(the task carried no text; the image attachment is the payload)");
  if (attachmentPaths.length > 0) {
    lines.push("", `[onlyne] attachment saved to: ${attachmentPaths.join(", ")}`);
  }
  return lines.join("\n");
}

/**
 * Task body for the `config_get` route: when a plugin does not declare `inject`
 * the host hands the payload over as `config_get{key:"stdin:<task text>"}`
 * (PROTOCOL.md, "Mounts and capabilities").
 * @returns {{ text: string } | null}
 */
export function stdinTaskText(args) {
  const key = args?.key;
  if (typeof key !== "string" || !key.startsWith("stdin:")) return null;
  return { text: key.slice("stdin:".length) };
}
