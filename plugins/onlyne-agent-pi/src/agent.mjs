// The agent side of the onlyne adapter protocol: one connection to the role
// workspace's client socket — v2 binds it in the machine-level runtime
// directory as `<digest>.sock`, not inside the tree (`socket.mjs`) — the
// hello/welcome handshake, assign delivery, turn-state reports, the host's
// nudge, the completion exit, probe, recycle and detach — plus reconnect when
// the client restarts under it.
//
// Everything pi-specific lives behind `surface` (see pi-surface.mjs): this
// module decides *what* the protocol says and hands the *effects* to the
// surface, which is why the whole state machine is testable against a plain
// Node unix socket.
//
// What the human sees travels through one funnel, `notice` below: the pi widget
// panel when the host has `ctx.ui.setWidget`, the footer status line plus a
// `[pi-onlyne]` stderr line when it has not. `log` keeps the diagnostics — a
// refused report, a socket error, a timeout, a framing fault — because those
// matter to a host with no panel at all.

import { readFileSync } from "node:fs";
import { createConnection } from "node:net";
import { createFrameDecoder, encodeFrame } from "./frame.mjs";
import { createActivity } from "./activity.mjs";
import {
  DEFAULT_HEARTBEAT_MS,
  assignAckArgs,
  completeReport,
  describePrincipal,
  detachArgs,
  heartbeatReport,
  helloArgs,
  headOf,
  hostBinding,
  imagePart,
  normalizeOutcome,
  readyReport,
  sendEnvelope,
  sessionRegisterArgs,
  SEQ_BASE,
  stdinTaskText,
  welcomeFrom,
} from "./protocol.mjs";

/** Reconnect ladder in milliseconds, capped like the client's own. */
export const RECONNECT_LADDER_MS = [1_000, 2_000, 4_000, 8_000, 16_000, 30_000];
/** How long the socket gets to answer the hello before it is torn down. */
export const HELLO_TIMEOUT_MS = 5_000;
/** Default bound on one request round trip. */
export const REQUEST_TIMEOUT_MS = 30_000;
/**
 * How long after an errored turn the plugin waits for `agent_settled` before it
 * reports the failure it witnessed itself.
 */
export const SETTLE_FALLBACK_MS = 2_000;

/** Capabilities this plugin implements on the wire. */
export const CAPABILITIES = ["register", "report", "inject", "recycle"];

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
   *   activity?: { note: (kind: string, text: string) => any, set: (patch: any) => any, lines: () => string[], events: any[] },
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
    /** The widget panel's model (`activity.mjs`); seeded with the local role. */
    this.activity = options.activity ?? createActivity();
    this.activity.set({ role: options.role });
    this.log = options.log ?? (() => {});
    this.capabilities = options.capabilities ?? CAPABILITIES;
    this.heartbeatMs = options.heartbeatMs ?? DEFAULT_HEARTBEAT_MS;
    this.ladder = options.ladder ?? RECONNECT_LADDER_MS;
    this.requestTimeoutMs = options.requestTimeoutMs ?? REQUEST_TIMEOUT_MS;
    this.helloTimeoutMs = options.helloTimeoutMs ?? HELLO_TIMEOUT_MS;
    this.settleFallbackMs = options.settleFallbackMs ?? SETTLE_FALLBACK_MS;
    this.createConnection = options.createConnection ?? ((path) => createConnection(path));
    // The pane this process was spawned in, reported on every heartbeat so the
    // supervisor board can attribute the tab (protocol.mjs `hostBinding`). Read
    // once: the environment of a process never changes. Injected by the tests,
    // which must not depend on the pane they run in.
    this.host = options.host !== undefined ? options.host : hostBinding(process.env);
    this.timer = options.timer ?? {
      set: (fn, ms) => setTimeout(fn, ms),
      clear: (handle) => clearTimeout(handle),
    };

    this.closed = false;
    this.connected = false;
    this.socket = null;
    this.welcome = null;
    /**
     * The generation for a beat whose task carries no record of its own (the
     * session's spawn task, before any assign wrote one). The generation the host
     * names in an `assign` lives on that task's record instead, where a second
     * concurrent assign cannot rewrite the first one's.
     */
    this.generation = 1;
    this.seq = SEQ_BASE;
    this.nextId = 1;
    /** @type {Map<number, { resolve: (value: any) => void, timer: any }>} */
    this.pending = new Map();
    this.attempt = 0;
    this.reconnectHandle = null;
    this.heartbeatHandle = null;
    this.settleHandle = null;
    /**
     * The heartbeat round in flight, and what the next pass of it should say.
     * `heartbeat` never lets two rounds overlap, so `beatRound` doubles as the
     * lock; `beatAgain` records that a beat was asked for while the round was
     * writing, and `beatPhase` what the newest of those asks wanted said — an
     * explicit phase, or null for "ask pi again". See `heartbeat`.
     */
    this.beatRound = null;
    this.beatAgain = false;
    this.beatPhase = null;
    /** @type {Map<string, any>} */
    this.tasks = new Map();
    /**
     * Completions the socket could not carry: one entry per `complete` call
     * taken while the connection was down, kept in the order they were called
     * and flushed on the next hello. A single slot would let a second
     * completion overwrite the first, so an outcome nobody had received would
     * vanish while a reconnect was already in flight.
     * @type {{ taskId: string, report: any, outcome: string, exitProcess: boolean }[]}
     */
    this.pendingCompletions = [];
    /**
     * The deliveries this process has already handed to the model, keyed by
     * envelope id (a delivery's own identity). Keying it by task id would swallow
     * every later envelope for a live task — the follow-up that never arrives —
     * and the guard below answers a true re-delivery with `duplicate`.
     */
    this.injectedDeliveries = new Set();
    this.deliveredProse = new Set();
    /** Pushes that arrived before the handshake finished; see `onFrame`. */
    this.handshaking = false;
    this.deferredPushes = [];
    this.agentState = "ready";
    this.lastError = null;
    /** Whether pi has already been asked to end this process (`exitSession`). */
    this.exitRequested = false;
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
    this.activity.set({ connection: "detached", taskId: null });
    this.notice("state", `detached (${reason})`);
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
      // The plugin's own report counter, and the seq each open task last
      // carried. A supervisor chasing a beat the host dropped as stale reads
      // these against the row's watermark instead of guessing from the log.
      seq: this.seq,
      taskSeqs: Object.fromEntries(
        [...this.tasks.values()].map((task) => [task.taskId, task.lastSeq ?? null]),
      ),
      tasks: this.activeTasks().map((task) => task.taskId),
      pendingCompletions: this.pendingCompletions.map((pending) => pending.taskId),
      lastError: this.lastError ? String(this.lastError.message ?? this.lastError) : null,
      activity: this.activity.events.slice(0, 5),
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
    this.activity.set({ connection: this.closed ? "detached" : "reconnecting" });
    this.notice("state", this.closed ? "detached" : "reconnecting");
    if (socket && !socket.destroyed) socket.destroy();
    this.scheduleReconnect();
  }

  scheduleReconnect() {
    if (this.closed || this.reconnectHandle) return;
    const delay = this.ladder[Math.min(this.attempt, this.ladder.length - 1)];
    this.attempt += 1;
    this.stats.reconnects += 1;
    this.reconnectHandle = this.timer.set(() => {
      this.reconnectHandle = null;
      if (!this.closed) this.connect();
    }, delay);
  }

  async onConnect() {
    this.attempt = 0;
    // From here until the welcome is adopted, pushes queue instead of running.
    this.handshaking = true;
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
    this.connected = true;
    // The client names every delivery it already handed to this session, so a
    // process that restarts under a live client does not re-inject work its
    // predecessor read: the delivery guard is process memory, and this is how it
    // survives one. Seeded before any push drains, so the first `assign` the new
    // connection sees is already deduped against it. Both spellings go in,
    // because the guard reads `envelope.id ?? task:<task_id>` (`onAssign`) and a
    // host may name either the delivery's own id or the task it belongs to here.
    const alreadyDelivered = Array.isArray(welcome.deliveredTasks) ? welcome.deliveredTasks : [];
    for (const taskId of alreadyDelivered) {
      this.injectedDeliveries.add(taskId);
      this.injectedDeliveries.add(`task:${taskId}`);
    }
    // A fresh connection has reported nothing, so the phase is a question, not
    // an answer: the first beat below re-derives it from pi. A taskless session
    // is the ready pool, and ready is its own state.
    this.agentState = this.tasks.size > 0 ? await this.derivedPhase() : "ready";
    this.activity.set({
      role: welcome.role,
      connection: "connected",
      generation: welcome.generation,
      // A reconnect inherits the task this process serves: the panel says so
      // from the first beat, with the assignment still queued behind the hello.
      taskId: this.activeTaskId() ?? null,
      phase: this.agentState,
    });
    this.notice("state", `connected role=${welcome.role} gen=${welcome.generation} host=${welcome.hostCapabilities.join(",")}`);
    const prose = welcome.prose.trim();
    if (prose && !this.deliveredProse.has(prose)) {
      this.deliveredProse.add(prose);
      this.surface.roleProse?.(prose);
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
    await this.flushPendingCompletions();
    // The frames that waited for the handshake: the welcome is adopted by now,
    // so the role prose is in the instruction layer before any assignment opens
    // a turn.
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
    else if (op === "nudge") this.onNudge(frame, args);
    else if (op === "probe") void this.probe();
    else if (op === "recycle") void this.onRecycle(args);
    else if (op === "config_get") void this.onConfigGet(args);
    else if (op === "bye") {
      this.notice("state", `host bye: ${args.reason ?? "unspecified"}`);
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

  /**
   * One human-facing event: record it, then draw the best host surface.
   * Hosts with `ctx.ui.setWidget` redraw the panel. Other hosts keep a footer
   * line and a `[pi-onlyne]` stderr line.
   *
   * @param {"in" | "dup" | "out" | "warn" | "state"} kind
   * @param {string} text
   */
  notice(kind, text) {
    this.activity.note(kind, text);
    if (this.surface.available?.widget) {
      this.surface.widget?.(this.activity.lines());
      return;
    }
    // No panel: the footer carries the steady header line, stderr the event.
    this.surface.status?.(this.activity.lines()[0]);
    this.log(`${kind}: ${text}`);
  }

  // --------------------------------------------------------------- reports

  async reportReady() {
    const taskId = this.envTaskId;
    if (!taskId || !this.connected) return;
    // The ready travels the one counter and moves the same row's watermark a
    // beat reads, so the task's record takes the clamp where one exists: a
    // ready landing between two of that task's beats cannot hand the row a seq
    // it has already seen.
    const seq = this.nextSeq(this.tasks.get(taskId) ?? null);
    try {
      await this.request("report", readyReport({
        taskId,
        sessionId: this.sessionId,
        generation: this.generation,
        seq,
      }));
      this.stats.reports += 1;
      this.notice("state", `ready ${taskId.slice(0, 8)}`);
    } catch (error) {
      this.log(`ready refused: ${error.message}`);
    }
  }

  /**
   * Allocate the next report sequence, for one task when the report names one.
   *
   * One writer gets one counter: the protocol asks for a sequence strictly
   * increasing for the life of a generation (`crates/onlyne-adapter/PROTOCOL.md`,
   * "Report sequencing and the ready barrier"), and the host's own events for a
   * row take `row.seq + 1` beside it (`onlyne-session`'s `reconcile/feed.rs`,
   * `reconcile/bridge.rs`). A per-task counter was the other option, and it is
   * the worse one: a task's row would then move exactly one per round, tying
   * with the host's interleaved writes to that row, and the reducer reads a tie
   * as `StaleOrDuplicateSeq` and throws the liveness fact away. Beating every
   * task off the one counter is what keeps each row ahead of the host.
   *
   * The gate itself is per task row, so the invariant a *task* owes is "my next
   * beat carries a higher seq than my last one". `task.lastSeq` makes that a
   * checked property rather than a side effect of write order: an allocation for
   * a task that has reported before is clamped above its own last report, so no
   * path — a ready between two beats, a second round folded into the first, a
   * follow-up envelope arriving mid-round — can hand a task back a seq its row
   * has already accepted.
   *
   * @param {any} [task] the task record the report speaks for. A report naming
   * no record (the task this process was spawned with, before any `assign` wrote
   * one) takes the bare counter, which is above everything it has ever sent.
   */
  nextSeq(task = null) {
    this.seq += 1;
    if (!task) return this.seq;
    if (typeof task.lastSeq === "number" && task.lastSeq >= this.seq) {
      this.seq = task.lastSeq + 1;
    }
    task.lastSeq = this.seq;
    return this.seq;
  }

  /**
   * One heartbeat round, or the round already running if one is in flight.
   *
   * Rounds never overlap. A beat is a snapshot of the session, so a second round
   * begun while the first is still writing holds no newer evidence than the one
   * in flight — it only splits one tick's facts across two sequences and doubles
   * the frames every task's row has to clear. The timer, a turn hook and a
   * `probe` can each ask for a beat in the same instant: the first starts the
   * round, the rest fold into it, and the round takes one more pass so the
   * newest ask is answered. A folded caller shares the running round's outcome,
   * rejection included, because a plugin reads a failed report as a link that
   * died, and the reconnect ladder — not a queue of beats behind a socket that
   * cannot carry them — is the answer to that.
   *
   * The phase is the one the phase rule names: `idle` only while the session
   * waits for user input, `running` for everything else, re-derived from pi once
   * per round so a phase that went stale when a run started again cannot survive
   * a tick — the answer is about the session, so a task does not get its own
   * view of it.
   *
   * @param {"idle" | "running" | null} [agent] the phase to state; absent means
   * ask pi.
   */
  heartbeat(agent = null) {
    if (this.beatRound) {
      // The newest ask wins the next pass, whatever it is. A bare `heartbeat()`
      // means "ask pi again" and so clears an earlier explicit phase: a
      // turn-start beat that says `running` must not pin the turn-end beat that
      // folded into it, or the idle a waiting session reached never gets
      // reported, and the client's own rule reads a working agent forever.
      this.beatAgain = true;
      this.beatPhase = agent;
      return this.beatRound;
    }
    this.beatPhase = agent ?? null;
    this.beatAgain = false;
    this.beatRound = this.runBeatRound();
    return this.beatRound;
  }

  /** The serialized driver behind `heartbeat`; see there for the why. */
  async runBeatRound() {
    let failure = null;
    try {
      for (;;) {
        this.beatAgain = false;
        try {
          await this.beatOnce(this.beatPhase);
        } catch (error) {
          // The link stopped answering mid-round. Later beats would only pile
          // behind frames this socket cannot carry; `dropSocket` rejects them
          // all and the reconnect path re-derives the phase anyway.
          failure = error;
          break;
        }
        if (!this.beatAgain) break;
      }
    } finally {
      this.beatRound = null;
      this.beatAgain = false;
    }
    if (failure) throw failure;
  }

  /**
   * One pass of the round: one beat for every task this session still holds.
   *
   * A session can hold two open tasks at once (the one it is working and the
   * next one already handed over), and a beat is the only liveness evidence
   * either row has: beating the head of the list alone let a second task sit
   * until the client's heartbeat timeout took it. Each beat carries its own
   * task's generation and a sequence strictly above the last one that task
   * sent, so an alternating pair of tasks can never be told stale by its own
   * earlier frame.
   *
   * A task this plugin already completed gets none: the beat is a full
   * snapshot of what the plugin can see, and after the completion the agent's
   * remaining turns are not this session's business — the row belongs to the
   * settlement the completion earned. The client rewrites the tuple's `delivery`
   * and `recovery` from its own records, so a late beat could no longer undo
   * the drain even if it sent one. The e2e case
   * `crates/onlyne-testkit/e2e/pi-live.sh` caught this race, where a turn-end
   * heartbeat left over from the finishing turn landed three milliseconds
   * behind the completion.
   */
  async beatOnce(agent) {
    if (!this.connected) return;
    let tasks = this.activeTasks();
    if (tasks.length === 0) {
      // Nothing is open: the beat still belongs to the task this process was
      // spawned with, unless that task is one this plugin already completed.
      const taskId = this.envTaskId;
      if (!taskId || this.tasks.get(taskId)?.completed) return;
      tasks = [{ taskId }];
    }
    const phase = agent ?? (await this.derivedPhase());
    this.agentState = phase;
    for (const task of tasks) {
      // The clamp is stamped on the record, where the next round can read it;
      // the synthetic entry above has none, and the bare counter covers it.
      const seq = this.nextSeq(this.tasks.get(task.taskId) ?? null);
      await this.request("report", heartbeatReport({
        taskId: task.taskId,
        generation: task.generation ?? this.generation,
        seq,
        agent: phase,
        host: this.host,
      }));
      this.stats.reports += 1;
    }
  }

  /**
   * Where the session actually is, in the only two words the wire has for it.
   * The surface asks pi, and adds the one case pi cannot see: work a
   * background-task extension took off the agent loop.
   * @returns {Promise<"idle"|"running">}
   */
  async derivedPhase() {
    try {
      const waiting = await this.surface.waitingForInput?.();
      return waiting === true ? "idle" : "running";
    } catch (error) {
      // A surface that cannot answer has not witnessed a session waiting for
      // input, and the rule reads that as running.
      this.log(`input-waiting probe failed: ${error.message}`);
      return "running";
    }
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
    // The delivery's own identity, so a re-offer of the same message is caught
    // and a genuinely new message for a running task gets through.
    const deliveryId = envelope.id ?? `task:${taskId}`;
    if (this.injectedDeliveries.has(deliveryId)) {
      this.stats.duplicates += 1;
      this.notice("dup", `~~ task ${taskId.slice(0, 8)} re-delivered, already injected`);
      await this.ack(taskId, true, "duplicate");
      return;
    }
    this.injectedDeliveries.add(deliveryId);
    this.stats.assigns += 1;

    // The delivery text arrives already rendered: the client's one template
    // built it from the sender, the body and the files it wrote itself. This
    // plugin injects those bytes and nothing else — it composes no wording of
    // its own around a delivery, and its other addition to what the model reads
    // is the role prose, which is a system-prompt section rather than a message.
    const text = typeof args.text === "string" ? args.text : "";
    const attachmentPaths = Array.isArray(args.attachments) ? args.attachments : [];
    const prose = typeof args.prose === "string" ? args.prose.trim() : "";
    const proseIsNew = prose.length > 0 && !this.deliveredProse.has(prose);
    if (proseIsNew) {
      this.deliveredProse.add(prose);
      this.surface.roleProse?.(prose);
    }

    const held = this.tasks.get(taskId);
    // A completed record is not a live one: a new envelope for a task that
    // already settled is fresh work under an old id, so it gets a fresh record.
    if (held && !held.completed) {
      // A new envelope for a task this session already holds — a follow-up, a
      // redirect, a bounce back through a handoff. The work record stays where
      // it is, and the failure an errored turn proved belonged to the
      // instruction being replaced, so it is cleared with that instruction.
      held.errored = false;
      held.envelopeId = envelope.id ?? held.envelopeId;
      // `lastSeq` deliberately does not reset here. It is the floor the next
      // allocation for this task must clear, and the host's row for the task
      // still holds the seq of the last beat it accepted; a follow-up envelope
      // opens no new row, so forgetting the floor would let a later round
      // re-issue a seq the reducer has already seen.
      // The host's newest word on this task's generation, which its next beat
      // must carry; a follow-up can move it.
      if (typeof args.generation === "number") held.generation = args.generation;
    } else {
      this.tasks.set(taskId, {
        taskId,
        envelopeId: envelope.id ?? null,
        /** The generation the host names for this task; the beat reports it. */
        generation: typeof args.generation === "number" ? args.generation : 1,
        /**
         * The seq of the last report this plugin sent for this task, stamped by
         * `nextSeq`. The host gates one task's row on its own watermark, so this
         * is what keeps a beat ahead of the frame before it however many tasks
         * share the plugin's counter. Null until the first report.
         */
        lastSeq: null,
        errored: false,
        head: "",
      });
    }
    this.agentState = "running";
    this.surface.wakeUser?.(text, this.imageParts(envelope));
    this.surface.customEntry?.("onlyne-assign", {
      taskId,
      envelopeId: envelope.id ?? null,
      kind: envelope.kind ?? "task",
      proseInjected: proseIsNew,
      prose,
      attachments: attachmentPaths,
    });
    this.activity.set({ taskId, phase: "running" });
    this.notice("in", `task ${taskId.slice(0, 8)} from ${describePrincipal(envelope.from)} (${envelope.kind ?? "task"}): ${headOf(envelope.body?.text)}`);
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

  /**
   * The host's turn-end nudge (`HostOp::Nudge`): the client owns the rule
   * (`docs/v2-CONTRACT.md` §3c) and the sentence it sends is the whole of what
   * the model gets.
   *
   * Handed over exactly as it arrived — no prefix, no count, no task id, no role
   * — which is what makes this a nudge rather than v1's ladder, whose reminder
   * re-injected the assignment and counted its rungs. Nothing in the session's
   * turn state is reset by it, and the plugin keeps no copy of the text.
   *
   * The answer claims only that the sentence was handed over, because the client
   * settles a delivery on it: `ok` when pi took the text, and a refusal when pi
   * would not. A frame carrying no `id` is a notification and gets no answer.
   *
   * @param {{ id?: number | null }} frame
   * @param {{ task_id?: string, text?: string }} args
   */
  onNudge(frame, args) {
    const text = typeof args.text === "string" ? args.text : "";
    const taskId = typeof args.task_id === "string" && args.task_id ? args.task_id.slice(0, 8) : "?";
    const handedOver = text.length > 0 && this.surface.wakeUser?.(text, []) === true;
    if (handedOver) this.notice("in", `nudge for task ${taskId}`);
    else this.notice("warn", `nudge for task ${taskId} was not handed to the model`);
    if (frame?.id === undefined || frame?.id === null) return;
    if (handedOver) {
      this.write({ reply_to: frame.id, ok: true });
      return;
    }
    this.write({
      reply_to: frame.id,
      ok: false,
      error: { code: "internal", message: "pi did not take the nudge text" },
    });
  }

  /** The host asked for a fresh observation: one heartbeat is the answer. */
  async probe() {
    try {
      await this.heartbeat();
      this.notice("state", "probe answered with a heartbeat");
    } catch (error) {
      this.log(`probe heartbeat refused: ${error.message}`);
    }
  }

  async onConfigGet(args) {
    const task = stdinTaskText(args);
    if (task) {
      // The no-`inject` route: the host hands the payload over as a config key.
      // The key carries the same rendered delivery text an `assign` would, so
      // it is injected as it stands — this route composes no wording either.
      this.notice("in", `stdin task: ${headOf(task.text)}`);
      this.surface.wakeUser?.(task.text, []);
      return;
    }
    this.log(`config_get ${args?.key ?? "?"} is not implemented by this plugin`);
  }

  async onRecycle(args) {
    this.stats.recycles += 1;
    const taskId = args.task_id ?? this.activeTaskId();
    this.notice("state", `recycled: ${args.reason ?? "operator"}`);
    if (taskId && args.outcome && this.tasks.get(taskId) && !this.tasks.get(taskId).completed) {
      await this.complete(taskId, args.outcome, `recycled: ${args.reason ?? "operator"}`, {
        exitProcess: false,
      }).catch((error) => this.log(`recycle completion refused: ${error.message}`));
    }
    this.stop(`recycle:${args.reason ?? "operator"}`);
    this.exitSession(args.reason ?? "recycle");
  }

  // ------------------------------------------------------------ pi → plugin

  /**
   * A turn started: the plugin's own agent fact is `running`.
   *
   * How a turn end settles the work is the client's rule
   * (`docs/v2-CONTRACT.md` §3c): the plugin counts nothing, bounds nothing, and
   * reports only the facts it alone can see — its own phase, and an errored
   * turn.
   */
  onTurnStart() {
    void this.heartbeat("running").catch((error) => this.log(`heartbeat refused: ${error.message}`));
  }

  /**
   * A turn ended: one turn of a run that may still have more. The phase is
   * re-derived, so the beat says `running` while pi keeps working and says
   * `idle` only once the session waits for input. A clean turn end settles
   * nothing: the client owns that decision, and it reaches the session as its
   * own frame.
   */
  onTurnEnd() {
    void this.heartbeat().catch((error) => this.log(`heartbeat refused: ${error.message}`));
  }

  /**
   * pi has settled: the session is waiting for input, which is the moment an
   * errored turn is reported from.
   */
  onSettled() {
    this.clearSettleFallback();
    void this.heartbeat().catch((error) => this.log(`heartbeat refused: ${error.message}`));
    void this.trySettle().catch((error) => this.log(`failed-turn report refused: ${error.message}`));
  }

  /**
   * Report a turn that failed, once pi is quiet. pi keeps `isIdle()` false while
   * it is running, retrying, compacting, or holding a queued continuation, and
   * a background task keeps work running after pi itself has settled, so a
   * signal that arrives during any of those waits re-arms instead of reporting
   * from a session that is still busy. The fallback timer and `agent_settled`
   * both land here, and a task is reported once because the report completes it.
   */
  async trySettle() {
    if (this.closed || this.activeTasks().length === 0) return;
    if (await this.derivedPhase() !== "idle") {
      this.armSettleFallback();
      return;
    }
    await this.reportFailedTurns().catch((error) => this.log(`failed-turn report refused: ${error.message}`));
  }

  /** A failed turn: the task's outcome is `failed`, with the error as its head. */
  onTurnError(text) {
    for (const task of this.tasks.values()) {
      if (task.completed) continue;
      task.errored = true;
      if (text) task.head = headOf(text);
    }
    void this.trySettle().catch((error) => this.log(`failed-turn report refused: ${error.message}`));
  }

  /**
   * The last assistant text seen, kept as the ledger head for a task whose
   * `onlyne_complete` call carries an empty `summary`.
   *
   * This is the fallback, never the deliverable: `onlyne_complete`'s `summary`
   * is reported byte for byte by `completeFromTool` and is never written back
   * here, so the sentence a turn happened to end on cannot stand in for the
   * summary the tool call carried. Nothing else reads it — and the full result
   * travels in `details`, which this never touches.
   */
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
      void this.trySettle().catch((error) => this.log(`failed-turn report refused: ${error.message}`));
    }, this.settleFallbackMs);
  }

  clearSettleFallback() {
    if (!this.settleHandle) return;
    this.timer.clear(this.settleHandle);
    this.settleHandle = null;
  }

  /**
   * Report the failures this session witnessed itself, for every task it still
   * holds.
   *
   * An errored turn is proof on its own — pi may skip the clean turn end — so
   * its task is reported `failed` at once, with the error as its head. Nothing
   * else is decided here: a turn that ends without a completion is the client's
   * case, and it reaches the session as the host's nudge (`onNudge`), so a
   * plugin that also decided it would be the second owner of the rule.
   */
  async reportFailedTurns() {
    for (const task of [...this.tasks.values()]) {
      if (task.completed || !task.errored) continue;
      await this.complete(task.taskId, "failed", task.head);
    }
  }

  /**
   * `onlyne_complete`: the model's explicit outcome, and the only path to
   * `done`.
   *
   * `summary` is the display line the ledger keeps: it is what the model handed
   * over, flattened to one line and reported as the `head`, so the last
   * assistant text never stands in for it. An empty `summary` carries no
   * display line at all and falls back to that assistant text.
   *
   * `details` is the full result and `files` the absolute paths it names. Both
   * travel on unchanged, and both are what the next hop and the originator
   * receive: the client owns the shape and the ceiling, and refuses an oversize
   * body with its own sentence (`docs/v2-CONTRACT.md` §3c).
   *
   * No policy of this plugin's runs before them. The completion's shape, the
   * details ceiling and the relay requirement are the client's checks, so its
   * refusal is raised out of here exactly as it arrived.
   *
   * @param {{ outcome?: string, summary?: string, details?: string, files?: string[] }} input
   */
  async completeFromTool(input = {}) {
    const task = [...this.tasks.values()].find((item) => !item.completed);
    const taskId = task?.taskId ?? this.envTaskId;
    if (!taskId) throw new Error("onlyne: no task is assigned to this session");
    const explicit = headOf(input.summary);
    const head = explicit || task?.head || "";
    return this.complete(taskId, normalizeOutcome(input.outcome), head, {
      details: typeof input.details === "string" ? input.details : null,
      files: Array.isArray(input.files) ? input.files : [],
    });
  }

  /**
   * Report the terminal fact. If the socket is down the report is remembered and
   * flushed on the next hello, so a completion survives a client restart.
   *
   * The `report` request is the durable handover, and it is the reason the exit
   * waits for its answer: the client replies from `serve_connection`
   * (`crates/onlyne-client/src/adapter_socket.rs`) only after `on_out` settled
   * the session row, acked the delivery and wrote the `Completion` envelope —
   * onto the server link, or into `client.db` intents when that link is down
   * (`crates/onlyne-client/src/dispatch.rs` `on_out`). A returned response
   * therefore means this process can leave without losing the outcome, and a
   * rejected or queued report must never exit.
   *
   * `details` and `files` ride on the report itself; `head` stays the display
   * line the ledger keeps.
   *
   * @param {{ exitProcess?: boolean, details?: string | null, files?: string[] }} [options]
   *   `exitProcess: false` is the recycle path, which ends the process after
   *   its own detach frame instead.
   */
  async complete(taskId, outcome, head, options = {}) {
    const exitProcess = options.exitProcess ?? true;
    const details = typeof options.details === "string" && options.details.length > 0 ? options.details : null;
    const files = Array.isArray(options.files) ? options.files : [];
    const normalized = normalizeOutcome(outcome);
    const summary = headOf(head);
    const task = this.tasks.get(taskId);
    if (task?.completed) return { taskId, outcome: normalized, head: summary, duplicate: true };
    if (task) task.completed = true;
    const report = completeReport({ taskId, outcome: normalized, head: summary, details, files });
    if (!this.connected) {
      this.pendingCompletions.push({ taskId, report, outcome: normalized, exitProcess });
      this.activity.set({ taskId, phase: `${normalized} queued` });
      this.notice("warn", `complete ${taskId.slice(0, 8)} ${normalized} queued: socket down`);
      return { taskId, outcome: normalized, head: summary, queued: true };
    }
    // The claim above only fences a second call while this report is in
    // flight. A refused or broken request handed nothing over, so the task
    // goes back to open and the caller sees why: a retry reports again, and
    // `completeFromTool` still finds the task.
    try {
      await this.request("report", report);
    } catch (error) {
      if (task) task.completed = false;
      throw error;
    }
    this.stats.completions += 1;
    this.surface.customEntry?.("onlyne-complete", { taskId, outcome: normalized, head: summary });
    this.activity.set({ taskId: this.activeTaskId() ?? null, phase: normalized });
    this.notice("out", `complete ${taskId.slice(0, 8)} ${normalized}${summary ? `: ${summary}` : ""}`);
    if (this.activeTasks().length === 0) {
      this.stopHeartbeat();
      if (exitProcess) this.exitSession(normalized);
    }
    return { taskId, outcome: normalized, head: summary };
  }

  /**
   * Ask pi to end the process this session runs in.
   *
   * One `ctx.shutdown()` per process: pi marks the request and runs its
   * teardown at `agent_settled` (`modes/interactive/interactive-mode.js`,
   * `modes/rpc/rpc-mode.js` in pi 0.85.1), which then emits `session_shutdown`
   * and that hook detaches this connection. The client daemon is unaffected:
   * the role runtime outlives every session it spawns.
   */
  exitSession(reason) {
    if (this.exitRequested) return;
    this.exitRequested = true;
    this.surface.exit?.(reason);
  }

  /**
   * Hand over every completion the dead socket could not carry, in the order the
   * calls came. The drain stops at the first report the new connection refuses:
   * that one goes back on the head of the queue, and the rest wait for the next
   * hello with it, so a client that dies mid-flush loses nothing.
   */
  async flushPendingCompletions() {
    while (this.pendingCompletions.length > 0) {
      if (!this.connected) return;
      const pending = this.pendingCompletions.shift();
      try {
        await this.request("report", pending.report);
        this.stats.completions += 1;
        this.activity.set({ taskId: this.activeTaskId() ?? null, phase: pending.outcome });
        this.notice("out", `complete ${pending.taskId.slice(0, 8)} ${pending.outcome} flushed after reconnect`);
        if (pending.exitProcess && this.activeTasks().length === 0) {
          this.exitSession(pending.outcome);
        }
      } catch (error) {
        this.pendingCompletions.unshift(pending);
        this.log(`queued completion still refused: ${error.message}`);
        break;
      }
    }
  }

  /**
   * One inline image read off the path the model named, in the wire's
   * `ImagePart` shape. `mimeForPath` refuses an extension outside the core's
   * four mime types, and `imagePart` refuses a payload above the byte ceiling.
   * @param {string | null | undefined} path
   */
  imageFromPath(path) {
    if (!path) return null;
    return imagePart({
      data: readFileSync(path),
      mime: mimeForPath(path),
      name: path.split("/").pop() ?? null,
    });
  }

  /**
   * `onlyne_send`: submit one envelope.
   * @param {{ to: string, text?: string, kind?: string, imagePath?: string | null }} input
   */
  async sendFromTool(input) {
    if (!this.connected) throw new Error("onlyne: client socket is not connected");
    const kind = input.kind === "task" ? "task" : "note";
    const image = this.imageFromPath(input.imagePath);
    const envelope = sendEnvelope({ from: this.role, to: input.to, kind, text: input.text ?? "", image });
    const data = await this.request("send", envelope);
    return { queued: true, op_id: envelope.op_id ?? null, kind, to: input.to, data };
  }

  /**
   * `onlyne_handoff`: hand this session's task on to the next hop of its family.
   *
   * The task named in the request is the session's own current one, so the child
   * the host mints under it is a continuation of the family this session serves:
   * the host reads the parent's causality, derives the child through
   * `Causality::child_of`, and answers the child's task id and hop. The client's
   * own refusal is raised out of here as it arrived.
   * @param {{ to: string, text?: string, imagePath?: string | null }} input
   */
  async handoffFromTool(input) {
    if (!this.connected) throw new Error("onlyne: client socket is not connected");
    const taskId = this.activeTaskId();
    if (!taskId) throw new Error("onlyne: no task is assigned to this session");
    const data = await this.request("handoff", {
      task_id: taskId,
      to: input.to,
      text: input.text ?? "",
      image: this.imageFromPath(input.imagePath),
    });
    const answer = data ?? {};
    return {
      taskId: answer.task_id ?? null,
      hop: answer.hop ?? null,
      queued: answer.queued ?? false,
      opId: answer.op_id ?? null,
      to: input.to,
    };
  }

  // ------------------------------------------------------------ attachments

  /**
   * The pi image parts for one delivery, from the envelope's inline image. The
   * client writes the files itself and names every path in `assign.args
   * .attachments`, so this plugin only hands pi the bytes it was given; the
   * model addresses the files through the paths the delivery text names.
   */
  imageParts(envelope) {
    const image = envelope?.body?.image;
    if (!image || typeof image.data_base64 !== "string") return [];
    const mime = typeof image.mime === "string" && image.mime ? image.mime : "image/png";
    const name = typeof image.name === "string" ? image.name : null;
    return [{ type: "image", mime, data: image.data_base64, name }];
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
