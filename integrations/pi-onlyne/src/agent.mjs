// The agent side of the onlyne adapter protocol: one connection to
// `<role workspace>/.onlyne/run/s`, the hello/welcome handshake, assign
// delivery, turn-state reports, the completion exit, probe, recycle and
// detach — plus reconnect when the client restarts under it.
//
// Everything pi-specific lives behind `surface` (see pi-surface.mjs): this
// module decides *what* the protocol says and hands the *effects* to the
// surface, which is why the whole state machine is testable against a plain
// Node unix socket.

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { createConnection } from "node:net";
import { join } from "node:path";
import { createFrameDecoder, encodeFrame } from "./frame.mjs";
import {
  DEFAULT_HEARTBEAT_MS,
  assignAckArgs,
  completeReport,
  detachArgs,
  heartbeatReport,
  helloArgs,
  headOf,
  imagePart,
  injectionText,
  normalizeOutcome,
  readyReport,
  sendEnvelope,
  sessionRegisterArgs,
  SEQ_BASE,
  stdinTaskText,
  welcomeFrom,
} from "./protocol.mjs";
import { paneClaim, publishPaneClaim } from "./attribution.mjs";

/** Reconnect ladder in milliseconds, capped like the client's own. */
export const RECONNECT_LADDER_MS = [1_000, 2_000, 4_000, 8_000, 16_000, 30_000];
/** How long the socket gets to answer the hello before it is torn down. */
export const HELLO_TIMEOUT_MS = 5_000;
/** Default bound on one request round trip. */
export const REQUEST_TIMEOUT_MS = 30_000;
/** How long after a turn end the plugin waits for `agent_settled` before it acts. */
export const SETTLE_FALLBACK_MS = 2_000;

/** Capabilities this plugin implements on the wire. */
export const CAPABILITIES = ["register", "report", "inject", "recycle"];

/** Mime guess for an attachment the host did not name. */
function extensionForMime(mime) {
  switch (mime) {
    case "image/png":
      return "png";
    case "image/jpeg":
      return "jpg";
    case "image/gif":
      return "gif";
    case "image/webp":
      return "webp";
    default:
      return "bin";
  }
}

/** Ids the host mints are uuids; anything else is flattened before it names a file. */
function safeSegment(value) {
  return String(value ?? "unknown").replace(/[^A-Za-z0-9._-]/g, "_").slice(0, 80);
}

export class OnlyneAgent {
  /**
   * @param {{
   *   socketPath: string,
   *   cwd: string,
   *   role: string,
   *   sessionId: string,
   *   taskId: string,
   *   surface: any,
   *   log?: (line: string, data?: unknown) => void,
   *   capabilities?: string[],
   *   heartbeatMs?: number,
   *   ladder?: number[],
   *   requestTimeoutMs?: number,
   *   helloTimeoutMs?: number,
   *   settleFallbackMs?: number,
   *   createConnection?: (path: string) => any,
   *   timer?: { set: (fn: () => void, ms: number) => any, clear: (handle: any) => void },
   * }} options
   */
  constructor(options) {
    this.socketPath = options.socketPath;
    this.cwd = options.cwd;
    this.role = options.role;
    this.sessionId = options.sessionId;
    this.envTaskId = options.taskId;
    this.surface = options.surface;
    this.log = options.log ?? (() => {});
    this.capabilities = options.capabilities ?? CAPABILITIES;
    this.heartbeatMs = options.heartbeatMs ?? DEFAULT_HEARTBEAT_MS;
    this.ladder = options.ladder ?? RECONNECT_LADDER_MS;
    this.requestTimeoutMs = options.requestTimeoutMs ?? REQUEST_TIMEOUT_MS;
    this.helloTimeoutMs = options.helloTimeoutMs ?? HELLO_TIMEOUT_MS;
    this.settleFallbackMs = options.settleFallbackMs ?? SETTLE_FALLBACK_MS;
    this.createConnection = options.createConnection ?? ((path) => createConnection(path));
    // Where this pane's binding goes (attribution.mjs). Injected by the tests,
    // which must not touch the workspace they run in.
    this.claimStore = options.claimStore ?? {
      publish: (claim) => publishPaneClaim({ workspace: options.cwd, claim }),
    };
    this.timer = options.timer ?? {
      set: (fn, ms) => setTimeout(fn, ms),
      clear: (handle) => clearTimeout(handle),
    };

    this.closed = false;
    this.connected = false;
    this.socket = null;
    this.welcome = null;
    this.generation = 1;
    this.seq = SEQ_BASE;
    this.nextId = 1;
    /** @type {Map<number, { resolve: (value: any) => void, timer: any }>} */
    this.pending = new Map();
    this.attempt = 0;
    this.reconnectHandle = null;
    this.heartbeatHandle = null;
    this.settleHandle = null;
    /** @type {Map<string, any>} */
    this.tasks = new Map();
    this.injectedTasks = new Set();
    this.deliveredProse = new Set();
    /** Pushes that arrived before the handshake finished; see `onFrame`. */
    this.handshaking = false;
    this.deferredPushes = [];
    this.agentState = "ready";
    this.lastError = null;
    this.stats = { assigns: 0, duplicates: 0, injections: 0, completions: 0, reports: 0, reconnects: 0, recycles: 0 };
  }

  // ---------------------------------------------------------------- lifecycle

  /** Open the connection and keep it open until `stop`. */
  start() {
    this.closed = false;
    this.connect();
  }

  /**
   * Leave: tell the host this plugin is going away, then stop reconnecting.
   * @param {string} reason
   */
  stop(reason = "quit") {
    if (this.closed) return;
    this.closed = true;
    this.clearClaim();
    this.clearTimers();
    const socket = this.socket;
    if (this.connected && socket && !socket.destroyed) {
      // Best effort: the detach frame is queued and the socket is ended so the
      // bytes leave before the FIN. pi does not wait for the host's answer.
      this.write({ id: this.nextId++, op: "detach", args: detachArgs(reason) });
      socket.end();
    } else if (socket && !socket.destroyed) {
      socket.destroy();
    }
    this.connected = false;
    this.socket = null;
    this.surface.status?.("onlyne: detached");
  }

  /**
   * Publish this pane's binding so the supervisor board can attribute the tab
   * (`integrations/orca-plugin/src/board.mjs`). A no-op outside an Orca pane,
   * and never fatal.
   */
  publishClaim(taskId = this.envTaskId) {
    this.claimStore.publish(paneClaim(process.env, { role: this.role, taskId }));
  }

  /** Drop the claim: this pane is no longer working on a task. */
  clearClaim() {
    this.claimStore.publish(null);
  }

  /** One line for `/onlyne status`. */
  status() {
    return {
      connected: this.connected,
      socket: this.socketPath,
      role: this.role,
      sessionId: this.sessionId,
      generation: this.generation,
      agentState: this.agentState,
      tasks: this.activeTasks().map((task) => task.taskId),
      pendingCompletion: this.pendingCompletion?.taskId ?? null,
      lastError: this.lastError ? String(this.lastError.message ?? this.lastError) : null,
      stats: { ...this.stats },
    };
  }

  // ------------------------------------------------------------- transport

  connect() {
    if (this.closed || this.socket) return;
    let socket;
    try {
      socket = this.createConnection(this.socketPath);
    } catch (error) {
      this.noteFailure(error);
      this.scheduleReconnect();
      return;
    }
    this.socket = socket;
    // The error listener goes on before anything else can throw: an `error` event
    // with no handler takes the whole pi process down, and a plugin bug must
    // never do that to its host.
    socket.on("error", (error) => this.noteFailure(error));
    socket.on("close", () => this.onClose(socket));
    let decoder;
    try {
      decoder = createFrameDecoder({
        onFrame: (frame) => this.onFrame(frame),
        onError: (error) => {
          this.log(`framing fault: ${error.message}`);
          this.noteFailure(error);
          this.dropSocket();
        },
      });
    } catch (error) {
      this.noteFailure(error);
      this.dropSocket();
      return;
    }
    socket.on("connect", () => {
      void this.onConnect();
    });
    socket.on("data", (chunk) => decoder.push(chunk));
  }

  onClose(socket) {
    if (this.socket !== socket) return;
    this.dropSocket();
  }

  /** Tear the current socket down and schedule the next attempt. */
  dropSocket() {
    const socket = this.socket;
    this.socket = null;
    this.connected = false;
    this.handshaking = false;
    this.deferredPushes = [];
    this.rejectAll("connection lost");
    this.stopHeartbeat();
    this.surface.status?.(this.closed ? "onlyne: detached" : "onlyne: reconnecting");
    if (socket && !socket.destroyed) socket.destroy();
    this.scheduleReconnect();
  }

  scheduleReconnect() {
    if (this.closed || this.reconnectHandle) return;
    const delay = this.ladder[Math.min(this.attempt, this.ladder.length - 1)];
    this.attempt += 1;
    this.stats.reconnects += 1;
    this.log(`reconnecting in ${delay}ms`);
    this.reconnectHandle = this.timer.set(() => {
      this.reconnectHandle = null;
      if (!this.closed) this.connect();
    }, delay);
  }

  async onConnect() {
    this.attempt = 0;
    // From here until the welcome is adopted, pushes queue instead of running.
    this.handshaking = true;
    this.log("connected; sending hello");
    const args = helloArgs({
      role: this.role,
      session: this.sessionId,
      taskId: this.envTaskId,
      pid: process.pid,
      capabilities: this.capabilities,
    });
    let body;
    try {
      body = await this.request("hello", args, { timeoutMs: this.helloTimeoutMs });
    } catch (error) {
      this.noteFailure(error);
      this.dropSocket();
      return;
    }
    const welcome = welcomeFrom(body);
    if (!welcome) {
      this.noteFailure(new Error("hello answered without a welcome"));
      this.dropSocket();
      return;
    }
    this.welcome = welcome;
    this.generation = welcome.generation;
    this.seq = SEQ_BASE;
    this.connected = true;
    this.agentState = this.tasks.size > 0 ? "idle" : "ready";
    this.surface.status?.(`onlyne: ${welcome.role}`);
    this.log(`welcome role=${welcome.role} generation=${welcome.generation} capabilities=${welcome.hostCapabilities.join(",")}`);
    this.surface.welcome?.(welcome);
    this.publishClaim();
    const prose = welcome.prose.trim();
    if (prose && !this.deliveredProse.has(prose)) {
      this.deliveredProse.add(prose);
      this.surface.proseContext?.(prose, welcome);
    }

    if (this.envTaskId) {
      await this.request("session_register", sessionRegisterArgs({
        sessionId: this.sessionId,
        taskId: this.envTaskId,
        generation: this.generation,
        pid: process.pid,
        title: `onlyne:${this.role}:${this.sessionId}`,
      })).catch((error) => this.log(`session_register refused: ${error.message}`));
    }
    await this.reportReady();
    if (this.pendingCompletion) await this.flushPendingCompletion();
    // The frames that waited for the handshake: the welcome is adopted by now,
    // so the role prose is context before any assignment opens a turn.
    this.handshaking = false;
    this.drainDeferred();
    if (this.tasks.size > 0) await this.heartbeat().catch(() => {});
    this.startHeartbeat();
  }

  /**
   * Run the pushes that arrived during the handshake, in arrival order. Called
   * once the welcome has been adopted; a socket that died first already
   * discarded the queue.
   */
  drainDeferred() {
    const queued = this.deferredPushes;
    this.deferredPushes = [];
    for (const frame of queued) this.onFrame(frame);
  }

  /**
   * Write one frame. Never throws: a dead socket is a reconnect, not a crash.
   * @param {any} frame
   */
  write(frame) {
    const socket = this.socket;
    if (!socket || socket.destroyed || !socket.writable) return false;
    try {
      socket.write(encodeFrame(frame));
      return true;
    } catch (error) {
      this.noteFailure(error);
      this.dropSocket();
      return false;
    }
  }

  /**
   * Send one request and wait for its response body.
   * @param {string} op
   * @param {any} args
   * @param {{ timeoutMs?: number }} [options]
   */
  request(op, args, options = {}) {
    const id = this.nextId++;
    const timeoutMs = options.timeoutMs ?? this.requestTimeoutMs;
    return new Promise((resolve, reject) => {
      const timer = this.timer.set(() => {
        this.pending.delete(id);
        reject(new Error(`${op} timed out after ${timeoutMs}ms`));
      }, timeoutMs);
      this.pending.set(id, { resolve, reject, timer });
      if (!this.write({ id, op, args })) {
        this.pending.delete(id);
        this.timer.clear(timer);
        reject(new Error(`${op} not delivered: socket is down`));
      }
    });
  }

  onFrame(frame) {
    if (!frame || typeof frame !== "object") return;
    if (frame.reply_to !== undefined && frame.reply_to !== null) {
      const entry = this.pending.get(frame.reply_to);
      if (!entry) return;
      this.pending.delete(frame.reply_to);
      this.timer.clear(entry.timer);
      if (frame.ok) entry.resolve(frame.data ?? null);
      else entry.reject(new Error(`${frame.error?.code ?? "error"}: ${frame.error?.message ?? "refused"}`));
      return;
    }
    // A push can share one TCP chunk with the hello reply, and the decoder
    // hands frames over in arrival order: without this gate an `assign` would
    // be injected before the welcome that carries the role prose and the
    // generation, which is the order the host's ready barrier (§6) assumes.
    // Pushes wait for the handshake; replies never do, or waiting would
    // deadlock the very request the handshake is waiting on.
    if (this.handshaking) {
      this.deferredPushes.push(frame);
      return;
    }
    const op = frame.op;
    const args = frame.args ?? {};
    if (op === "assign") void this.onAssign(args);
    else if (op === "probe") void this.probe();
    else if (op === "recycle") void this.onRecycle(args);
    else if (op === "config_get") void this.onConfigGet(args);
    else if (op === "bye") {
      this.log(`host bye: ${args.reason ?? "unspecified"}`);
      // The session ended, but pi may live on to be handed another task, so the
      // claim goes with the session rather than with the socket.
      this.clearClaim();
      this.dropSocket();
    } else if (op) this.log(`ignoring host op ${op}`);
  }

  rejectAll(reason) {
    for (const [id, entry] of this.pending) {
      this.pending.delete(id);
      this.timer.clear(entry.timer);
      entry.reject(new Error(reason));
    }
  }

  noteFailure(error) {
    this.lastError = error;
    this.log(`socket error: ${error?.message ?? error}`);
  }

  // --------------------------------------------------------------- reports

  async reportReady() {
    const taskId = this.envTaskId;
    if (!taskId || !this.connected) return;
    this.seq += 1;
    try {
      await this.request("report", readyReport({
        taskId,
        sessionId: this.sessionId,
        generation: this.generation,
        seq: this.seq,
      }));
      this.stats.reports += 1;
      this.log(`ready reported for ${taskId}`);
    } catch (error) {
      this.log(`ready refused: ${error.message}`);
    }
  }

  /**
   * One heartbeat for the task this connection serves.
   *
   * A task this plugin already completed gets none. `observed` is a full
   * snapshot, so a heartbeat sent after the completion report would put
   * `delivery: none` and `outcome: pending` back over the terminal tuple the
   * host derived from it, and the reducer accepts that snapshot: the session
   * would read `idle` again after having read `exited`. The e2e case
   * `crates/onlyne-testkit/e2e/pi-live.sh` caught exactly this race, where a
   * turn-end heartbeat left over from the finishing turn landed three
   * milliseconds behind the completion.
   */
  async heartbeat(agent = this.agentState) {
    if (!this.connected) return;
    const taskId = this.activeTaskId();
    if (!taskId) return;
    if (this.tasks.get(taskId)?.completed) return;
    this.agentState = agent;
    this.seq += 1;
    await this.request("report", heartbeatReport({
      taskId,
      generation: this.generation,
      seq: this.seq,
      agent,
    }));
    this.stats.reports += 1;
  }

  /** Tasks still owing a completion; a finished task keeps its record. */
  activeTasks() {
    return [...this.tasks.values()].filter((task) => !task.completed);
  }

  /** The task this connection is working on right now, if any. */
  activeTaskId() {
    return this.activeTasks()[0]?.taskId ?? this.envTaskId;
  }

  startHeartbeat() {
    if (this.heartbeatHandle) return;
    this.heartbeatHandle = this.timer.set(() => {
      this.heartbeatHandle = null;
      if (!this.closed && this.connected) {
        void this.heartbeat().catch((error) => this.log(`heartbeat refused: ${error.message}`));
      }
      this.startHeartbeat();
    }, this.heartbeatMs);
  }

  stopHeartbeat() {
    if (!this.heartbeatHandle) return;
    this.timer.clear(this.heartbeatHandle);
    this.heartbeatHandle = null;
  }

  clearTimers() {
    this.stopHeartbeat();
    if (this.reconnectHandle) {
      this.timer.clear(this.reconnectHandle);
      this.reconnectHandle = null;
    }
    if (this.settleHandle) {
      this.timer.clear(this.settleHandle);
      this.settleHandle = null;
    }
    this.rejectAll("agent stopped");
  }

  // ------------------------------------------------------------ host → plugin

  async onAssign(args) {
    const envelope = args.envelope ?? {};
    const taskId = args.task_id ?? envelope.causality?.task ?? null;
    if (!taskId) {
      this.log("assign carried no task id; ignored");
      return;
    }
    if (this.injectedTasks.has(taskId)) {
      this.stats.duplicates += 1;
      this.log(`assign for ${taskId} already injected; acking without a second injection`);
      await this.ack(taskId, true, "duplicate");
      return;
    }
    this.injectedTasks.add(taskId);
    this.stats.assigns += 1;
    this.publishClaim(taskId);
    if (typeof args.generation === "number") this.generation = args.generation;

    const attachments = this.writeAttachments(taskId, envelope);
    const prose = typeof args.prose === "string" ? args.prose.trim() : "";
    const proseIsNew = prose.length > 0 && !this.deliveredProse.has(prose);
    if (proseIsNew) this.deliveredProse.add(prose);
    const text = injectionText({ assign: { ...args, task_id: taskId }, proseIsNew, attachmentPaths: attachments.map((item) => item.path) });

    this.tasks.set(taskId, {
      taskId,
      envelopeId: envelope.id ?? null,
      turnsSinceAssign: 0,
      turns: 0,
      errored: false,
      head: "",
      failed: false,
    });
    this.agentState = "running";
    this.log(`assign ${taskId} from ${JSON.stringify(envelope.from ?? null)}; injecting ${text.length} chars`);
    this.surface.wakeUser?.(text, attachments.map((item) => item.part));
    this.surface.customEntry?.("onlyne-assign", {
      taskId,
      envelopeId: envelope.id ?? null,
      kind: envelope.kind ?? "task",
      proseInjected: proseIsNew,
      prose,
      attachments: attachments.map((item) => item.path),
    });
    this.surface.status?.(`onlyne: ${taskId.slice(0, 8)} running`);
    this.startHeartbeat();
    await this.ack(taskId, true, null);
  }

  async ack(taskId, accepted, reason) {
    try {
      await this.request("assign_ack", assignAckArgs({ taskId, accepted, reason }));
    } catch (error) {
      this.log(`assign_ack refused: ${error.message}`);
    }
  }

  /** The host asked for a fresh observation: one heartbeat is the answer. */
  async probe() {
    try {
      await this.heartbeat();
      this.log("probe answered with a heartbeat");
    } catch (error) {
      this.log(`probe heartbeat refused: ${error.message}`);
    }
  }

  async onConfigGet(args) {
    const task = stdinTaskText(args);
    if (task) {
      // The no-`inject` route: the host hands the payload over as a config key.
      this.log("task body received through config_get/stdin");
      this.surface.wakeUser?.(`[onlyne] task body (delivered as stdin):\n\n${task.text}`, []);
      return;
    }
    this.log(`config_get ${args?.key ?? "?"} is not implemented by this plugin`);
  }

  async onRecycle(args) {
    this.stats.recycles += 1;
    const taskId = args.task_id ?? this.activeTaskId();
    this.log(`recycle task=${taskId ?? "?"} reason=${args.reason ?? "?"} outcome=${args.outcome ?? "-"}`);
    if (taskId && args.outcome && this.tasks.get(taskId) && !this.tasks.get(taskId).completed) {
      await this.complete(taskId, args.outcome, `recycled: ${args.reason ?? "operator"}`).catch((error) =>
        this.log(`recycle completion refused: ${error.message}`),
      );
    }
    this.stop(`recycle:${args.reason ?? "operator"}`);
    this.surface.exit?.(args.reason ?? "recycle");
  }

  // ------------------------------------------------------------ pi → plugin

  /** A turn started: the plugin's own agent fact is `running`. */
  onTurnStart() {
    const task = [...this.tasks.values()].find((item) => !item.completed);
    if (task) task.turns += 1;
    void this.heartbeat("running").catch((error) => this.log(`heartbeat refused: ${error.message}`));
  }

  /** A turn ended: the agent is idle, and the settle window starts. */
  onTurnEnd() {
    for (const task of this.tasks.values()) {
      if (!task.completed) task.turnsSinceAssign += 1;
    }
    void this.heartbeat("idle").catch((error) => this.log(`heartbeat refused: ${error.message}`));
    this.armSettleFallback();
  }

  /** pi will not continue on its own: run the completion exit. */
  onSettled() {
    this.clearSettleFallback();
    this.trySettle();
  }

  /**
   * The one completion trigger. pi keeps `isIdle()` false while it is running,
   * retrying, compacting, or holding a queued continuation, so a settle signal
   * that arrives during any of those waits instead of reporting a premature
   * outcome.
   */
  trySettle() {
    if (this.closed || this.activeTasks().length === 0) return;
    if (this.surface.isIdle?.() === false) {
      this.armSettleFallback();
      return;
    }
    void this.settleNow().catch((error) => this.log(`completion failed: ${error.message}`));
  }

  /** A failed turn: the task's outcome is `failed` unless it already ended. */
  onTurnError(text) {
    for (const task of this.tasks.values()) {
      if (task.completed) continue;
      task.failed = true;
      task.errored = true;
      if (text) task.head = headOf(text);
    }
    this.trySettle();
  }

  /** The last assistant text seen, kept as the completion summary. */
  noteAssistantText(text) {
    const flat = headOf(text);
    if (!flat) return;
    for (const task of this.tasks.values()) {
      if (!task.completed) task.head = flat;
    }
  }

  armSettleFallback() {
    this.clearSettleFallback();
    if (this.closed || this.activeTasks().length === 0) return;
    this.settleHandle = this.timer.set(() => {
      this.settleHandle = null;
      this.trySettle();
    }, this.settleFallbackMs);
  }

  clearSettleFallback() {
    if (!this.settleHandle) return;
    this.timer.clear(this.settleHandle);
    this.settleHandle = null;
  }

  /**
   * The auto outcome rule: every active task whose turn produced output ends
   * `done` (or `failed` when the turn errored), with the last assistant text as
   * its head. A task assigned but not yet turned is left alone: the injected
   * message has not run yet, and completing now would lie.
   */
  async settleNow() {
    for (const task of [...this.tasks.values()]) {
      if (task.completed) continue;
      // A turn has to have run: a full turn end is the ordinary proof, and an
      // errored turn is proof enough on its own (pi may skip the clean turn_end).
      if (task.turnsSinceAssign === 0 && !task.errored) continue;
      await this.complete(task.taskId, task.failed ? "failed" : "done", task.head);
    }
  }

  /**
   * `onlyne_complete`: an explicit outcome from the model, which wins over the
   * auto rule.
   * @param {{ outcome?: string, text?: string }} input
   */
  async completeFromTool(input = {}) {
    const task = [...this.tasks.values()].find((item) => !item.completed);
    const taskId = task?.taskId ?? this.envTaskId;
    if (!taskId) throw new Error("onlyne: no task is assigned to this session");
    return this.complete(taskId, normalizeOutcome(input.outcome), input.text ?? task?.head ?? "");
  }

  /**
   * Report the terminal fact. If the socket is down the report is remembered and
   * flushed on the next hello, so a completion survives a client restart.
   */
  async complete(taskId, outcome, head) {
    const normalized = normalizeOutcome(outcome);
    const summary = headOf(head);
    const task = this.tasks.get(taskId);
    if (task?.completed) return { taskId, outcome: normalized, head: summary, duplicate: true };
    if (task) task.completed = true;
    const report = completeReport({ taskId, outcome: normalized, head: summary });
    if (!this.connected) {
      this.pendingCompletion = { taskId, report };
      this.surface.status?.(`onlyne: ${taskId.slice(0, 8)} ${normalized} (queued)`);
      this.log(`completion for ${taskId} queued: socket is down`);
      return { taskId, outcome: normalized, head: summary, queued: true };
    }
    await this.request("report", report);
    this.stats.completions += 1;
    this.surface.status?.(`onlyne: ${taskId.slice(0, 8)} ${normalized}`);
    this.surface.customEntry?.("onlyne-complete", { taskId, outcome: normalized, head: summary });
    this.log(`completion ${taskId} ${normalized} head=${JSON.stringify(summary.slice(0, 60))}`);
    if (this.activeTasks().length === 0) this.stopHeartbeat();
    return { taskId, outcome: normalized, head: summary };
  }

  async flushPendingCompletion() {
    const pending = this.pendingCompletion;
    if (!pending || !this.connected) return;
    this.pendingCompletion = null;
    try {
      await this.request("report", pending.report);
      this.stats.completions += 1;
      this.log(`queued completion for ${pending.taskId} flushed after reconnect`);
    } catch (error) {
      this.pendingCompletion = pending;
      this.log(`queued completion still refused: ${error.message}`);
    }
  }

  /**
   * `onlyne_send`: submit one envelope.
   * @param {{ to: string, text?: string, kind?: string, imagePath?: string | null }} input
   */
  async sendFromTool(input) {
    if (!this.connected) throw new Error("onlyne: client socket is not connected");
    const kind = input.kind === "task" ? "task" : "note";
    let image = null;
    if (input.imagePath) {
      const bytes = readFileSync(input.imagePath);
      image = imagePart({ data: bytes, mime, name: input.imagePath.split("/").pop() ?? null });
    }
    const envelope = sendEnvelope({ from: this.role, to: input.to, kind, text: input.text ?? "", image });
    const data = await this.request("send", envelope);
    return { queued: true, op_id: envelope.op_id ?? null, kind, to: input.to, data };
  }

  // ------------------------------------------------------------ attachments

  /**
   * Write one inbound image to the workspace and return the path plus the pi
   * image part, so the model both sees the picture and can address the file.
   */
  writeAttachments(taskId, envelope) {
    const image = envelope?.body?.image;
    if (!image || typeof image.data_base64 !== "string") return [];
    const mime = typeof image.mime === "string" && image.mime ? image.mime : "image/png";
    const name = safeSegment(image.name ?? `image.${extensionForMime(mime)}`);
    const dir = join(this.cwd, ".onlyne", "tmp", "attachments");
    const path = join(dir, `${safeSegment(taskId)}-${safeSegment(envelope.id)}-${name}`);
    try {
      const bytes = Buffer.from(image.data_base64, "base64");
      mkdirSync(dir, { recursive: true, mode: 0o700 });
      writeFileSync(path, bytes, { mode: 0o600 });
      this.log(`attachment written to ${path} (${bytes.length} bytes)`);
      return [{ path, part: { type: "image", mime, data: image.data_base64, name } }];
    } catch (error) {
      this.log(`attachment write failed: ${error.message}`);
      return [];
    }
  }
}

/** Mime type for one attachment path the model asked to send. */
export function mimeForPath(path) {
  const lower = String(path).toLowerCase();
  if (lower.endsWith(".png")) return "image/png";
  if (lower.endsWith(".jpg") || lower.endsWith(".jpeg")) return "image/jpeg";
  if (lower.endsWith(".gif")) return "image/gif";
  if (lower.endsWith(".webp")) return "image/webp";
  throw new Error(`onlyne: unsupported image type for ${path}`);
}
