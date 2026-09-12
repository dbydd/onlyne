// Agent state-machine tests against a fake host on a real unix socket: the same
// framing, the same frames, no Rust process. The assign frame is the host's own
// wire vector, so the input side is byte-identical to what the client sends.

import assert from "node:assert/strict";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, test } from "node:test";

import { OnlyneAgent } from "./agent.mjs";
import { createFrameDecoder, encodeFrame } from "./frame.mjs";
import { SEQ_BASE, readyReport } from "./protocol.mjs";

const VECTOR_DIR = fileURLToPath(new URL("../../../crates/onlyne-proto/tests/wire_vectors/", import.meta.url));
const ASSIGN_FRAME = existsSync(VECTOR_DIR)
  ? JSON.parse(JSON.parse(readFileSync(`${VECTOR_DIR}adapter_host_assign.json`, "utf8")).frame)
  : null;
const TASK_ID = "11111111-1111-4111-8111-111111111111";
const SESSION_ID = "8b1c";
const PANE_KEY = "45e603f7-0772-48aa-bcf6-832272747713:b6d067b6-9255-4f5c-a13f-24f194ea0560";
const PANE_TAB = "45e603f7-0772-48aa-bcf6-832272747713";
const PANE_LEAF = "b6d067b6-9255-4f5c-a13f-24f194ea0560";
/** Exactly the ORCA_* environment an Orca pane exports (measured on 1.4.198). */
const PANE_ENV = {
  ORCA_PANE_KEY: PANE_KEY,
  ORCA_TAB_ID: PANE_TAB,
  ORCA_LEAF_ID: PANE_LEAF,
  ORCA_TERMINAL_HANDLE: "term_1",
};

/** The `observed` bodies of every heartbeat the fake host received. */
function heartbeats(host) {
  return host
    .of("report")
    .filter((args) => args.kind === "heartbeat")
    .map((args) => args.data.observed);
}

/** Poll until `predicate` holds, so a test never races the event loop. */
async function waitFor(predicate, { timeoutMs = 2_000, stepMs = 5 } = {}) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = predicate();
    if (value) return value;
    if (Date.now() > deadline) throw new Error("timed out waiting for the expected state");
    await new Promise((resolve) => setTimeout(resolve, stepMs));
  }
}

/**
 * A minimal host: records every frame it receives, answers every request, and
 * can push notifications at any time. `coalesceAssign` puts an assignment in
 * the same chunk as the hello reply, the way a client handing over a staged
 * session does when both frames are ready at once. `holdCompletion` keeps
 * `report` requests carrying a `complete` unanswered until `releaseReports`,
 * which is the window a test needs to prove what the plugin does not do before
 * the client has acknowledged the outcome. `failSend` refuses every `send` the
 * way a client with no route to the target would. The ready and heartbeat
 * reports are never held: the handshake waits on them.
 */
class FakeHost {
  constructor({ coalesceAssign = null, holdCompletion = false, failSend = false } = {}) {
    this.server = createServer((socket) => this.onConnection(socket));
    this.frames = [];
    this.sockets = new Set();
    this.connections = 0;
    this.coalesceAssign = coalesceAssign;
    this.holdCompletion = holdCompletion;
    this.failSend = failSend;
    this.held = [];
  }

  listen(path) {
    return new Promise((resolve) => this.server.listen(path, resolve));
  }

  onConnection(socket) {
    this.connections += 1;
    this.sockets.add(socket);
    socket.on("error", () => {});
    socket.on("close", () => this.sockets.delete(socket));
    const decoder = createFrameDecoder({
      onFrame: (frame) => this.onFrame(socket, frame),
      onError: () => socket.destroy(),
    });
    socket.on("data", (chunk) => decoder.push(chunk));
  }

  onFrame(socket, frame) {
    this.frames.push({ frame, socket });
    if (frame.id === undefined) return;
    let body = { ok: true, data: null };
    if (frame.op === "hello") {
      const mount = frame.args.mount ?? {};
      body = {
        ok: true,
        data: {
          op: "welcome",
          args: {
            protocol: 1,
            role: mount.role,
            session_id: mount.session ?? SESSION_ID,
            generation: 1,
            prose: "Read the incoming task",
            server: { connected: true, cluster: "local", name: "server" },
            host_capabilities: ["probe", "recycle"],
          },
        },
      };
    }
    if (this.holdCompletion && frame.op === "report" && frame.args?.kind === "complete") {
      this.held.push({ socket, id: frame.id });
      return;
    }
    if (this.failSend && frame.op === "send") {
      socket.write(encodeFrame({
        reply_to: frame.id,
        ok: false,
        error: { code: "no_route", message: "no route to that role" },
      }));
      return;
    }
    const reply = encodeFrame({ reply_to: frame.id, ...body });
    const push = frame.op === "hello" && this.coalesceAssign
      ? encodeFrame({ op: "assign", args: this.coalesceAssign })
      : null;
    socket.write(push ? Buffer.concat([reply, push]) : reply);
  }

  /** Answer the completion reports held so far, the way the client would. */
  releaseReports() {
    const held = this.held;
    this.held = [];
    for (const entry of held) {
      entry.socket.write(encodeFrame({ reply_to: entry.id, ok: true, data: null }));
    }
  }

  notify(op, args) {
    for (const socket of this.sockets) socket.write(encodeFrame({ op, args }));
  }

  of(op) {
    return this.frames.filter((entry) => entry.frame.op === op).map((entry) => entry.frame.args);
  }

  close() {
    for (const socket of this.sockets) socket.destroy();
    return new Promise((resolve) => this.server.close(resolve));
  }
}

/** The effect surface the agent drives, recorded for assertions. */
function fakeSurface(options = {}) {
  const calls = { wakeUser: [], prose: [], entries: [], status: [], exits: [] };
  return {
    calls,
    available: {
      wakeUser: true,
      proseContext: true,
      customEntry: true,
      status: true,
      exit: true,
      isIdle: true,
      registerTool: true,
      registerCommand: true,
    },
    wakeUser: (text, parts = []) => {
      calls.wakeUser.push({ text, parts });
      return true;
    },
    proseContext: (text) => {
      calls.prose.push(text);
      return true;
    },
    customEntry: (type, data) => {
      calls.entries.push({ type, data });
      return true;
    },
    status: (text) => calls.status.push(text),
    welcome: () => {},
    isIdle: () => (options.idle === undefined ? true : options.idle()),
    exit: (reason) => calls.exits.push(reason),
  };
}

/** One agent on a fresh temp workspace socket. */
async function startAgent({
  surface = fakeSurface(),
  capabilities,
  options = {},
  coalesceAssign = null,
  holdCompletion = false,
  failSend = false,
  relay = null,
} = {}) {
  const dir = mkdtempSync(join(tmpdir(), "pi-onlyne-agent-"));
  const socketPath = join(dir, "s");
  const host = new FakeHost({ coalesceAssign, holdCompletion, failSend });
  await host.listen(socketPath);
  const logs = [];
  const agent = new OnlyneAgent({
    socketPath,
    cwd: dir,
    role: "planner",
    sessionId: SESSION_ID,
    taskId: TASK_ID,
    surface,
    log: (line) => logs.push(line),
    ladder: [20, 40, 80],
    // The fallback is the agent_settled-less path; a test that wants it asks for
    // a short window, and every other test drives the exit explicitly.
    settleFallbackMs: options.settleFallbackMs ?? 30_000,
    heartbeatMs: options.heartbeatMs ?? 60_000,
    ...(capabilities ? { capabilities } : {}),
    ...(relay ? { relay } : {}),
    ...(options.host !== undefined ? { host: options.host } : {}),
  });
  cleanups.push(async () => {
    agent.stop("test");
    await host.close();
    rmSync(dir, { recursive: true, force: true });
  });
  return { agent, host, surface, dir, socketPath, logs };
}

const cleanups = [];
afterEach(async () => {
  while (cleanups.length > 0) await cleanups.pop()();
});

const assignArgs = () => (ASSIGN_FRAME ? ASSIGN_FRAME.args : {
  envelope: {
    protocol: 1,
    id: "3f2a1c4e-5b6d-4e7f-8a90-1b2c3d4e5f60",
    kind: "task",
    from: { role: { role: "planner" } },
    to: { role: { role: "builder" } },
    causality: { task: TASK_ID, hop: 0, attempt: 0 },
    body: { text: "build it" },
    ts: "2026-09-10T12:00:00Z",
    admin: false,
  },
  prose: "Read the incoming task",
  task_id: TASK_ID,
  generation: 1,
});

/**
 * Run `body` with exactly the ORCA_* environment an Orca pane exports, so the
 * result does not depend on the shell the tests happen to run in, then restore
 * whatever was there.
 */
async function inOrcaPane(env, body) {
  const touched = new Set([...Object.keys(env), ...Object.keys(process.env).filter((key) => key.startsWith("ORCA_"))]);
  const saved = new Map([...touched].map((key) => [key, process.env[key]]));
  for (const key of touched) delete process.env[key];
  Object.assign(process.env, env);
  try {
    return await body();
  } finally {
    for (const [key, value] of saved) {
      if (value === undefined) delete process.env[key];
      else process.env[key] = value;
    }
  }
}

/** One mounted agent: the socket is up and the ready report has landed, so a
 * push from the test cannot race the dial. */
async function mountedAgent(options = {}) {
  const started = await startAgent(options);
  started.agent.start();
  await waitFor(() => (started.host.of("report").length >= 1 ? true : null));
  return started;
}

test("every heartbeat names the Orca pane this process was spawned in", async () => {
  await inOrcaPane(PANE_ENV, async () => {
    const { host } = await mountedAgent();
    host.notify("assign", assignArgs());
    await waitFor(() => (host.of("assign_ack").length === 1 ? true : null));

    host.notify("probe", {});
    const [observed] = await waitFor(() => (heartbeats(host).length ? heartbeats(host) : null));
    assert.deepEqual(observed.host, {
      orca: { pane_key: PANE_KEY, tab_id: PANE_TAB, leaf_id: PANE_LEAF, handle: "term_1" },
    });
    // The binding rides beside the state dimensions instead of replacing them.
    assert.equal(observed.agent, "running");
    assert.equal(observed.public, "working");
    assert.equal(observed.version.generation, 1);
  });
});

test("a reconnect reports the same pane", async () => {
  await inOrcaPane(PANE_ENV, async () => {
    const { agent, host } = await mountedAgent();
    host.notify("assign", assignArgs());
    await waitFor(() => (host.of("assign_ack").length === 1 ? true : null));
    host.notify("probe", {});
    await waitFor(() => (heartbeats(host).length ? true : null));

    // The binding is process state: a client restarting under the plugin cannot
    // move the pane the process runs in.
    for (const socket of [...host.sockets]) socket.destroy();
    await waitFor(() => (host.connections >= 2 ? true : null));
    await waitFor(() => (agent.connected === true ? true : null));
    const reports = await waitFor(() => (heartbeats(host).length >= 2 ? heartbeats(host) : null));
    assert.deepEqual(reports.at(-1).host, reports[0].host);
  });
});

test("a second task in the same pane reports the same pane", async () => {
  await inOrcaPane(PANE_ENV, async () => {
    const { host } = await mountedAgent();
    host.notify("assign", assignArgs());
    await waitFor(() => (host.of("assign_ack").length === 1 ? true : null));
    host.notify("probe", {});
    await waitFor(() => (heartbeats(host).length ? true : null));

    // The binding belongs to the process, not to the task: handing this pane
    // another task cannot change where the process runs.
    host.notify("assign", { ...assignArgs(), task_id: "22222222-2222-4222-8222-222222222222" });
    await waitFor(() => (host.of("assign_ack").length === 2 ? true : null));
    host.notify("probe", {});
    const reports = await waitFor(() => (heartbeats(host).length >= 2 ? heartbeats(host) : null));
    assert.deepEqual(reports.at(-1).host, reports[0].host);
  });
});

test("a pi outside an Orca pane reports no host at all", async () => {
  await inOrcaPane({}, async () => {
    const { host } = await mountedAgent();
    host.notify("assign", assignArgs());
    await waitFor(() => (host.of("assign_ack").length === 1 ? true : null));
    host.notify("probe", {});

    const [observed] = await waitFor(() => (heartbeats(host).length ? heartbeats(host) : null));
    assert.equal("host" in observed, false, "no pane, no binding");
    assert.equal(observed.agent, "running", "the tuple is still a full observation");
  });
});

test("a pane that exports no handle reports the pane without inventing one", async () => {
  await inOrcaPane({ ORCA_PANE_KEY: PANE_KEY }, async () => {
    const { host } = await mountedAgent();
    host.notify("assign", assignArgs());
    await waitFor(() => (host.of("assign_ack").length === 1 ? true : null));
    host.notify("probe", {});

    const [observed] = await waitFor(() => (heartbeats(host).length ? heartbeats(host) : null));
    // The pane key carries both ids; the handle is a field the environment
    // either has or has not, and a missing one is absent rather than empty.
    assert.deepEqual(observed.host, {
      orca: { pane_key: PANE_KEY, tab_id: PANE_TAB, leaf_id: PANE_LEAF },
    });
  });
});

test("the binding is the environment the process was spawned with", async () => {
  await inOrcaPane(PANE_ENV, async () => {
    const { host } = await mountedAgent();
    // A later change to the environment cannot move the process: what is
    // reported is the pane it was started in.
    process.env.ORCA_PANE_KEY = "aaaa1111-1111-4111-8111-111111111111:bbbb2222-2222-4222-8222-222222222222";
    host.notify("assign", assignArgs());
    await waitFor(() => (host.of("assign_ack").length === 1 ? true : null));
    host.notify("probe", {});

    const [observed] = await waitFor(() => (heartbeats(host).length ? heartbeats(host) : null));
    assert.equal(observed.host.orca.pane_key, PANE_KEY);
  });
});

test("a session writes nothing under the workspace: the binding is protocol data", async () => {
  await inOrcaPane(PANE_ENV, async () => {
    const { host, dir } = await mountedAgent();
    host.notify("assign", assignArgs());
    await waitFor(() => (host.of("assign_ack").length === 1 ? true : null));
    host.notify("probe", {});
    await waitFor(() => (heartbeats(host).length ? true : null));

    assert.equal(
      existsSync(join(dir, ".onlyne")),
      false,
      "the plugin keeps no cache file; the pane travels in the observation"
    );
  });
});

test("a fresh agent opens with hello, registers and reports ready", async () => {
  const { agent, host, surface } = await startAgent();
  agent.start();
  const [hello] = await waitFor(() => (host.of("hello").length === 1 ? host.of("hello") : null));
  assert.deepEqual(hello, {
    protocol: 1,
    plugin: "pi-onlyne",
    version: "1.0.0",
    kind: "agent",
    capabilities: ["register", "report", "inject", "recycle"],
    mount: { role: "planner", session: SESSION_ID, task_id: TASK_ID, pid: process.pid },
  });

  const [register] = await waitFor(() => (host.of("session_register").length === 1 ? host.of("session_register") : null));
  assert.equal(register.session_id, SESSION_ID);
  assert.equal(register.task_id, TASK_ID);
  assert.equal(register.generation, 1);

  const [ready] = await waitFor(() => (host.of("report").length >= 1 ? host.of("report") : null));
  assert.equal(ready.kind, "ready");
  assert.equal(ready.data.task_id, TASK_ID);
  assert.equal(ready.data.session_id, SESSION_ID);
  assert.equal(ready.data.generation, 1);
  // The report stream starts above the host's own dispatch sequence, or the
  // reducer would drop it as stale (onlyne-session's watermark gate).
  assert.ok(ready.data.seq > SEQ_BASE - 1, `seq ${ready.data.seq} must clear the host watermark`);
  // The role prose arrived once, as context rather than as a turn.
  assert.deepEqual(surface.calls.prose, ["Read the incoming task"]);
  assert.equal(agent.status().connected, true);
});

test("an assign is injected once, acked, and a redelivery changes nothing", async () => {
  const { agent, host, surface } = await startAgent();
  agent.start();
  await waitFor(() => host.of("report").length >= 1);

  host.notify("assign", assignArgs());
  await waitFor(() => (surface.calls.wakeUser.length === 1 ? true : null));
  const injected = surface.calls.wakeUser[0];
  assert.match(injected.text, /\[onlyne\] task 11111111-1111-4111-8111-111111111111 from role:planner \(kind task\)/);
  assert.match(injected.text, /build it/);
  assert.deepEqual(injected.parts, []);
  // The prose came with welcome and is not repeated on every payload.
  assert.deepEqual(surface.calls.prose, ["Read the incoming task"]);
  assert.doesNotMatch(injected.text, /Read the incoming task/);
  assert.deepEqual(surface.calls.entries[0].type, "onlyne-assign");
  assert.equal(surface.calls.entries[0].data.proseInjected, false);

  const acks = await waitFor(() => (host.of("assign_ack").length >= 1 ? host.of("assign_ack") : null));
  assert.deepEqual(acks[0], { task_id: TASK_ID, accepted: true });
  assert.deepEqual(agent.status().tasks, [TASK_ID]);

  // The same delivery again: one injection, one more ack that says why.
  host.notify("assign", assignArgs());
  const second = await waitFor(() => (host.of("assign_ack").length >= 2 ? host.of("assign_ack") : null));
  assert.equal(surface.calls.wakeUser.length, 1);
  assert.deepEqual(second[1], { task_id: TASK_ID, accepted: true, reason: "duplicate" });
});

// The live case in crates/onlyne-testkit/e2e/pi-live.sh found this: the client
// hands over a staged session by writing the hello reply and the first assign
// together, so both frames arrive in one read. The assignment must not be
// injected from inside the handshake, or the role prose loses its welcome-time
// delivery and gets folded into the task turn instead.
test("an assign sharing the hello reply's chunk waits for the welcome", async () => {
  const events = [];
  const surface = fakeSurface();
  const proseContext = surface.proseContext;
  const wakeUser = surface.wakeUser;
  surface.proseContext = (text, welcome) => {
    events.push("prose");
    return proseContext(text, welcome);
  };
  surface.wakeUser = (text, parts) => {
    events.push("assign");
    return wakeUser(text, parts);
  };
  const { agent, host, logs } = await startAgent({ surface, coalesceAssign: assignArgs() });
  agent.start();
  try {
    await waitFor(() => (surface.calls.wakeUser.length === 1 ? true : null));
  } catch (error) {
    throw new Error(`${error.message}; agent log: ${logs.join(" | ")}`);
  }
  assert.deepEqual(events, ["prose", "assign"]);
  assert.deepEqual(surface.calls.prose, ["Read the incoming task"]);
  assert.doesNotMatch(surface.calls.wakeUser[0].text, /Read the incoming task/);
  assert.equal(surface.calls.entries[0].data.proseInjected, false);
  const acks = await waitFor(() => (host.of("assign_ack").length >= 1 ? host.of("assign_ack") : null));
  assert.deepEqual(acks[0], { task_id: TASK_ID, accepted: true });
});

test("turn hooks report running then idle, and a settle completes done with the head", async () => {
  const { agent, host, surface } = await startAgent();
  agent.start();
  await waitFor(() => host.of("report").length >= 1);
  host.notify("assign", assignArgs());
  await waitFor(() => (surface.calls.wakeUser.length === 1 ? true : null));

  agent.onTurnStart();
  const running = await waitFor(() => host.of("report").find((report) => report.data?.observed?.agent === "running"));
  assert.equal(running.kind, "heartbeat");
  assert.equal(running.data.observed.public, "working");
  assert.equal(running.data.observed.version.generation, 1);

  agent.onTurnEnd();
  const idle = await waitFor(() => host.of("report").find((report) => report.data?.observed?.agent === "idle"));
  assert.equal(idle.data.observed.public, "idle");

  agent.noteAssistantText("OK");
  agent.onSettled();
  const complete = await waitFor(() => host.of("report").find((report) => report.kind === "complete"));
  assert.deepEqual(complete, { kind: "complete", data: { task_id: TASK_ID, outcome: "done", head: "OK" } });
  assert.deepEqual(agent.status().tasks, []);
  // The completion ends the session: the process that ran it is asked to leave.
  assert.deepEqual(surface.calls.exits, ["done"]);
  // Heartbeats stop with the last task, so a settled session stops writing.
  const reports = host.of("report").length;
  await new Promise((resolve) => setTimeout(resolve, 120));
  assert.equal(host.of("report").length, reports);
});

// The live case found this ordering too: pi's turn-end hook fires in the same
// millisecond as the settle that reports the completion, so the turn-end
// heartbeat lands after the completion report. `observed` is a whole snapshot,
// and one carrying `delivery: none`/`outcome: pending` puts the session back to
// `idle` after the host has already recorded `exited`.
test("a turn-end heartbeat after the completion never leaves the plugin", async () => {
  const { agent, host, surface } = await startAgent();
  agent.start();
  await waitFor(() => host.of("report").length >= 1);
  host.notify("assign", assignArgs());
  await waitFor(() => (surface.calls.wakeUser.length === 1 ? true : null));
  agent.onTurnStart();
  await waitFor(() => host.of("report").find((report) => report.data?.observed?.agent === "running"));

  agent.noteAssistantText("OK");
  await agent.complete(TASK_ID, "done", "OK");
  const reports = host.of("report").length;
  agent.onTurnEnd();
  agent.onSettled();
  await new Promise((resolve) => setTimeout(resolve, 60));
  assert.deepEqual(host.of("report").slice(reports), [], "the completion is the last report");
});

test("a busy pi holds the completion until it is idle", async () => {
  let idle = false;
  const surface = fakeSurface({ idle: () => idle });
  const { agent, host } = await startAgent({ surface, options: { settleFallbackMs: 40 } });
  agent.start();
  await waitFor(() => host.of("report").length >= 1);
  host.notify("assign", assignArgs());
  await waitFor(() => (surface.calls.wakeUser.length === 1 ? true : null));
  agent.onTurnEnd();
  agent.noteAssistantText("still working");
  agent.onSettled();
  await new Promise((resolve) => setTimeout(resolve, 80));
  assert.deepEqual(host.of("report").filter((report) => report.kind === "complete"), []);

  idle = true;
  const complete = await waitFor(() => host.of("report").find((report) => report.kind === "complete"));
  assert.equal(complete.data.head, "still working");
});

test("an assigned task that never ran is not completed", async () => {
  const { agent, host, surface } = await startAgent();
  agent.start();
  await waitFor(() => host.of("report").length >= 1);
  host.notify("assign", assignArgs());
  await waitFor(() => (surface.calls.wakeUser.length === 1 ? true : null));
  // No turn ran: a settle now would claim work that never happened.
  agent.onSettled();
  await new Promise((resolve) => setTimeout(resolve, 100));
  assert.deepEqual(host.of("report").filter((report) => report.kind === "complete"), []);
  assert.deepEqual(agent.status().tasks, [TASK_ID]);
});

test("a failed turn settles the task as failed with the error as head", async () => {
  const { agent, host, surface } = await startAgent();
  agent.start();
  await waitFor(() => host.of("report").length >= 1);
  host.notify("assign", assignArgs());
  await waitFor(() => (surface.calls.wakeUser.length === 1 ? true : null));
  agent.onTurnStart();
  agent.onTurnError("provider exploded");
  const complete = await waitFor(() => host.of("report").find((report) => report.kind === "complete"));
  assert.deepEqual(complete.data, { task_id: TASK_ID, outcome: "failed", head: "provider exploded" });
});

test("an explicit tool outcome wins and a second completion is refused", async () => {
  const { agent, host, surface } = await startAgent();
  agent.start();
  await waitFor(() => host.of("report").length >= 1);
  host.notify("assign", assignArgs());
  await waitFor(() => (surface.calls.wakeUser.length === 1 ? true : null));
  agent.onTurnStart();
  agent.onTurnEnd();
  agent.noteAssistantText("looks fine");

  const result = await agent.completeFromTool({ outcome: "failed", text: "changed my mind" });
  assert.deepEqual(result, { taskId: TASK_ID, outcome: "failed", head: "changed my mind" });
  assert.deepEqual(await agent.completeFromTool({ outcome: "done" }), {
    taskId: TASK_ID,
    outcome: "done",
    head: "",
    duplicate: true,
  });
  agent.onSettled();
  await new Promise((resolve) => setTimeout(resolve, 60));
  const completes = host.of("report").filter((report) => report.kind === "complete");
  assert.equal(completes.length, 1);
  assert.equal(completes[0].data.outcome, "failed");
  // One session asks for one exit, even when a second completion is refused.
  assert.deepEqual(surface.calls.exits, ["failed"]);
});

// The completion body is what the tool call handed over. The sentence a turn
// ends on is only the fallback for a task whose argument carries nothing, and
// the auto rule reports exactly that fallback field — so an explicit argument
// can never be displaced, in either call path.
test("an explicit tool argument is the head over the last assistant text", async () => {
  const { agent, host, surface } = await startAgent();
  agent.start();
  await waitFor(() => host.of("report").length >= 1);
  host.notify("assign", assignArgs());
  await waitFor(() => (surface.calls.wakeUser.length === 1 ? true : null));
  agent.onTurnStart();
  agent.onTurnEnd();
  agent.noteAssistantText("Handed off to `a` with K=6.");

  const payload = "K=10: 1:a 2:b 3:c 4:d 5:e 6:a 7:b 8:c 9:d 10:e";
  const result = await agent.completeFromTool({ outcome: "done", text: payload });
  assert.equal(result.head, payload, "the argument is the head, byte for byte");
  const completes = host.of("report").filter((report) => report.kind === "complete");
  assert.deepEqual(completes[0].data, { task_id: TASK_ID, outcome: "done", head: payload });

  // The model keeps talking until the exit, so the sentence that follows the
  // call must not take the head's place: the task is settled, and it is
  // reported once.
  agent.noteAssistantText("done");
  agent.onSettled();
  await new Promise((resolve) => setTimeout(resolve, 60));
  assert.equal(host.of("report").filter((report) => report.kind === "complete").length, 1);
});

// An argument that carries nothing is no argument: the completion summary is
// the last assistant text instead of an empty head. (An argument absent
// altogether is the queued-completion case above.)
test("an empty tool argument falls back to the last assistant text", async () => {
  for (const text of ["", "   "]) {
    const { agent, host, surface } = await startAgent();
    agent.start();
    await waitFor(() => host.of("report").length >= 1);
    host.notify("assign", assignArgs());
    await waitFor(() => (surface.calls.wakeUser.length === 1 ? true : null));
    agent.onTurnStart();
    agent.onTurnEnd();
    agent.noteAssistantText("appended 10:e and handed the token back");

    const result = await agent.completeFromTool({ outcome: "done", text });
    assert.equal(result.head, "appended 10:e and handed the token back", `text=${JSON.stringify(text)}`);
    const complete = host.of("report").find((report) => report.kind === "complete");
    assert.equal(complete.data.head, "appended 10:e and handed the token back");
  }
});

// The exit is a consequence of the client's acknowledgement, not of the
// completion call: the client answers a `report` only after it has settled the
// session row and written the `Completion` envelope, so a report still in
// flight must leave the process running (this is the frame the client
// duplicates into its intent queue when its own link is down).
test("the exit waits for the client's acknowledgement of the completion report", async () => {
  const { agent, host, surface } = await startAgent({ holdCompletion: true });
  agent.start();
  await waitFor(() => host.of("report").length >= 1);
  host.notify("assign", assignArgs());
  await waitFor(() => (surface.calls.wakeUser.length === 1 ? true : null));
  agent.onTurnStart();
  agent.onTurnEnd();
  agent.noteAssistantText("OK");

  const completion = agent.complete(TASK_ID, "done", "OK");
  await waitFor(() =>
    host.of("report").some((report) => report.kind === "complete") ? true : null,
  );
  await new Promise((resolve) => setTimeout(resolve, 40));
  assert.deepEqual(
    surface.calls.exits,
    [],
    "an unacknowledged completion must not take the process down",
  );

  host.releaseReports();
  await completion;
  assert.deepEqual(surface.calls.exits, ["done"]);
});

// A session that only ever reported `running` and then completed leaves the
// ledger saying `running` forever: the client settles the row from the
// completion report and hears nothing more. One final observation, sent after
// the completion is acknowledged and before the process leaves, is what makes
// an exited session read idle.
test("a completion after a running beat publishes the settled observation before the exit", async () => {
  const { agent, host, surface } = await startAgent();
  agent.start();
  await waitFor(() => host.of("report").length >= 1);
  host.notify("assign", assignArgs());
  await waitFor(() => (surface.calls.wakeUser.length === 1 ? true : null));
  agent.onTurnStart();
  await waitFor(() =>
    host.of("report").find((report) => report.data?.observed?.agent === "running"),
  );
  agent.noteAssistantText("OK");

  await agent.complete(TASK_ID, "done", "OK");
  const reports = host.of("report").filter((report) => report.kind !== "ready");
  const kinds = reports.map((report) => report.kind);
  assert.deepEqual(
    kinds.slice(-2),
    ["complete", "heartbeat"],
    "the settled observation follows the completion: " + JSON.stringify(kinds),
  );
  const settled = reports.at(-1).data.observed;
  assert.equal(settled.agent, "idle");
  assert.equal(settled.outcome, "done");
  assert.equal(settled.delivery, "accepted");
  assert.equal(settled.recovery, "draining");
  assert.equal(settled.public, "exited");
  assert.deepEqual(surface.calls.exits, ["done"]);
});

test("a session whose last beat was idle publishes no extra observation", async () => {
  const { agent, host, surface } = await startAgent();
  agent.start();
  await waitFor(() => host.of("report").length >= 1);
  host.notify("assign", assignArgs());
  await waitFor(() => (surface.calls.wakeUser.length === 1 ? true : null));
  agent.onTurnStart();
  agent.onTurnEnd();
  await waitFor(() => host.of("report").find((report) => report.data?.observed?.agent === "idle"));
  agent.noteAssistantText("OK");
  const before = host.of("report").filter((report) => report.kind === "heartbeat").length;

  await agent.complete(TASK_ID, "done", "OK");
  assert.equal(
    host.of("report").filter((report) => report.kind === "heartbeat").length,
    before,
    "an already-idle session needs no second idle observation",
  );
  assert.deepEqual(surface.calls.exits, ["done"]);
});

test("a queued completion and its settled observation flush in order after reconnect", async () => {
  const { agent, host, surface } = await startAgent();
  agent.start();
  await waitFor(() => host.of("report").length >= 1);
  host.notify("assign", assignArgs());
  await waitFor(() => (surface.calls.wakeUser.length === 1 ? true : null));
  agent.onTurnStart();
  agent.onTurnEnd();
  agent.noteAssistantText("done offline");

  for (const socket of [...host.sockets]) socket.destroy();
  await waitFor(() => (agent.status().connected === false ? true : null));
  await agent.completeFromTool({ outcome: "done" });
  assert.deepEqual(surface.calls.exits, [], "nothing exits before the report lands");

  await waitFor(() => (host.connections >= 2 ? true : null));
  await waitFor(() => (surface.calls.exits.length === 1 ? true : null));
  const reports = host.of("report").filter((report) => report.kind !== "ready");
  assert.deepEqual(
    reports.slice(-2).map((report) => [report.kind, report.data.observed?.agent ?? null]),
    [
      ["complete", null],
      ["heartbeat", "idle"],
    ],
    "the settled observation rides after the flushed completion: " + JSON.stringify(reports),
  );
  assert.equal(reports.at(-1).data.observed.outcome, "done");
});

test("an inbound image is written under the workspace and handed to pi", async () => {
  const { agent, host, surface, dir } = await startAgent();
  agent.start();
  await waitFor(() => host.of("report").length >= 1);
  const bytes = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  const args = assignArgs();
  args.envelope.body = { text: "what is this?", image: { data_base64: bytes.toString("base64"), mime: "image/png", name: "shot.png" } };
  host.notify("assign", args);
  await waitFor(() => (surface.calls.wakeUser.length === 1 ? true : null));

  const injected = surface.calls.wakeUser[0];
  assert.equal(injected.parts.length, 1);
  assert.equal(injected.parts[0].mime, "image/png");
  assert.equal(injected.parts[0].data, bytes.toString("base64"));
  const path = join(dir, ".onlyne", "tmp", "attachments", `${TASK_ID}-3f2a1c4e-5b6d-4e7f-8a90-1b2c3d4e5f60-shot.png`);
  assert.ok(existsSync(path), `expected ${path}`);
  assert.deepEqual([...readFileSync(path)], [...bytes]);
  assert.ok(injected.text.includes(path), "the injected message names the file it wrote");
  assert.deepEqual(surface.calls.entries[0].data.attachments, [path]);
});

test("a probe is answered with a heartbeat for the live task", async () => {
  const { agent, host, surface } = await startAgent();
  agent.start();
  await waitFor(() => host.of("report").length >= 1);
  host.notify("assign", assignArgs());
  await waitFor(() => (surface.calls.wakeUser.length === 1 ? true : null));

  host.notify("probe", { task_id: TASK_ID });
  const heartbeat = await waitFor(() =>
    host.of("report").find((report) => report.kind === "heartbeat" && report.data.task_id === TASK_ID),
  );
  assert.equal(heartbeat.data.observed.agent, "running");
  assert.ok(heartbeat.data.seq > SEQ_BASE, "each heartbeat advances the plugin's own sequence");
});

test("a task body handed over as config_get still reaches pi", async () => {
  const { agent, host, surface } = await startAgent();
  agent.start();
  await waitFor(() => host.of("report").length >= 1);
  host.notify("config_get", { key: "stdin:do the thing" });
  await waitFor(() => (surface.calls.wakeUser.length === 1 ? true : null));
  assert.match(surface.calls.wakeUser[0].text, /do the thing/);
});

test("recycle settles the task, detaches and asks pi to exit", async () => {
  const { agent, host, surface } = await startAgent();
  agent.start();
  await waitFor(() => host.of("report").length >= 1);
  host.notify("assign", assignArgs());
  await waitFor(() => (surface.calls.wakeUser.length === 1 ? true : null));

  host.notify("recycle", { task_id: TASK_ID, reason: "operator", outcome: "cancelled" });
  const complete = await waitFor(() => host.of("report").find((report) => report.kind === "complete"));
  assert.equal(complete.data.outcome, "cancelled");
  const detach = await waitFor(() => (host.of("detach").length >= 1 ? host.of("detach") : null));
  assert.equal(detach[0].reason, "recycle:operator");
  assert.deepEqual(surface.calls.exits, ["operator"]);
  const connections = host.connections;
  await new Promise((resolve) => setTimeout(resolve, 120));
  assert.equal(host.connections, connections, "a recycled agent must not reconnect");
});

test("a completion reported while the socket is down is flushed after reconnect", async () => {
  const { agent, host, surface } = await startAgent();
  agent.start();
  await waitFor(() => host.of("report").length >= 1);
  host.notify("assign", assignArgs());
  await waitFor(() => (surface.calls.wakeUser.length === 1 ? true : null));
  agent.onTurnStart();
  agent.onTurnEnd();
  agent.noteAssistantText("done offline");

  for (const socket of [...host.sockets]) socket.destroy();
  await waitFor(() => (agent.status().connected === false ? true : null));
  const queued = await agent.completeFromTool({ outcome: "done" });
  assert.equal(queued.queued, true);
  assert.deepEqual(
    surface.calls.exits,
    [],
    "a completion this process could not hand over keeps it alive",
  );

  await waitFor(() => (host.connections >= 2 ? true : null));
  const complete = await waitFor(() => host.of("report").find((report) => report.kind === "complete"));
  assert.equal(complete.data.head, "done offline");
  assert.equal(agent.status().connected, true, "the plugin re-hellos after a disconnect");
  const hellos = host.of("hello");
  // The flusher's acknowledgement is the handover the queued report was
  // waiting for, so the process leaves once it lands.
  await waitFor(() => (surface.calls.exits.length === 1 ? true : null));
  assert.deepEqual(surface.calls.exits, ["done"]);
  assert.equal(hellos.length, 2);
  assert.deepEqual(hellos[1].mount, hellos[0].mount);
});

test("stop detaches and leaves no reconnect behind", async () => {
  const { agent, host } = await startAgent();
  agent.start();
  await waitFor(() => host.of("report").length >= 1);
  agent.stop("pi:quit");
  await waitFor(() => (host.of("detach").length >= 1 ? host.of("detach") : null));
  assert.equal(host.of("detach")[0].reason, "pi:quit");
  const connections = host.connections;
  await new Promise((resolve) => setTimeout(resolve, 120));
  assert.equal(host.connections, connections);
  assert.equal(agent.status().connected, false);
});

test("a reduced capability set reaches the hello unchanged", async () => {
  const { agent, host } = await startAgent({ capabilities: ["register", "report", "recycle"] });
  agent.start();
  const [hello] = await waitFor(() => (host.of("hello").length === 1 ? host.of("hello") : null));
  assert.deepEqual(hello.capabilities, ["register", "report", "recycle"]);
});

test("a host that never answers the hello is dropped and retried", async () => {
  const dir = mkdtempSync(join(tmpdir(), "pi-onlyne-mute-"));
  const socketPath = join(dir, "s");
  const accepted = new Set();
  const server = createServer((socket) => {
    accepted.add(socket);
    socket.on("error", () => {});
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  const logs = [];
  const agent = new OnlyneAgent({
    socketPath,
    cwd: dir,
    role: "planner",
    sessionId: SESSION_ID,
    taskId: TASK_ID,
    surface: fakeSurface(),
    log: (line) => logs.push(line),
    ladder: [20],
    helloTimeoutMs: 40,
    settleFallbackMs: 30,
  });
  cleanups.push(async () => {
    agent.stop("test");
    // The host holds the connection open, so `server.close` alone would wait
    // forever: the sockets go first.
    for (const socket of accepted) socket.destroy();
    await new Promise((resolve) => server.close(resolve));
    rmSync(dir, { recursive: true, force: true });
  });
  agent.start();
  await waitFor(() => (logs.some((line) => line.includes("reconnecting")) ? true : null));
  assert.equal(agent.status().connected, false);
  assert.match(agent.status().lastError, /hello timed out/);
});

// ---------------------------------------------------------------- relay guard

// The relay guard is what stops a session from reporting a terminal outcome
// while it still owes a downstream handoff. Its evidence is delivery only:
// the roles this session's own successful `onlyne_send` calls reached.

/** The exact `report.complete` args this plugin sent before the guard existed. */
const COMPLETE_VECTOR =
  '{"kind":"complete","data":{"task_id":"11111111-1111-4111-8111-111111111111","outcome":"done","head":"handed the token over"}}';

/** Every `report.complete` the fake host received, in arrival order. */
function completions(host) {
  return host.of("report").filter((report) => report.kind === "complete");
}

/** The host's own assign frame, handed over by a role that is not this one. */
function assignmentFrom(role) {
  const args = assignArgs();
  return { ...args, envelope: { ...args.envelope, from: { role: { role } } } };
}

test("with no relay policy the completion frame is unchanged, force and reason included", async () => {
  const inputs = [
    { outcome: "done", text: "handed the token over" },
    { outcome: "done", text: "handed the token over", force: true, reason: "no policy is in force" },
  ];
  for (const input of inputs) {
    const { agent, host } = await startAgent();
    agent.start();
    await waitFor(() => host.of("report").length >= 1);
    await agent.completeFromTool(input);
    assert.equal(JSON.stringify(completions(host)[0]), COMPLETE_VECTOR, JSON.stringify(input));
  }
});

test("a relay list refuses a completion until every named role has a handoff", async () => {
  const { agent, host, surface } = await startAgent({ relay: { required: ["writer"] } });
  agent.start();
  await waitFor(() => host.of("report").length >= 1);

  await assert.rejects(
    () => agent.completeFromTool({ outcome: "done", text: "wrote the notes" }),
    (error) => {
      assert.match(
        error.message,
        /^onlyne: relay guard: missing handoff to: writer \(this session delivered to: none\)/,
      );
      assert.match(error.message, /force:true and a non-empty reason/);
      return true;
    },
  );
  assert.deepEqual(completions(host), []);
  assert.deepEqual(surface.calls.exits, []);

  await agent.sendFromTool({ to: "writer", text: "here is the outline" });
  const result = await agent.completeFromTool({ outcome: "done", text: "wrote the notes" });
  assert.deepEqual(result, { taskId: TASK_ID, outcome: "done", head: "wrote the notes" });
  assert.equal(completions(host).length, 1);
  assert.deepEqual(surface.calls.exits, ["done"]);
});

test("a relay count wants distinct downstream roles and ignores echoes", async () => {
  const { agent, host } = await startAgent({ relay: { required: [], count: 2 } });
  agent.start();
  await waitFor(() => host.of("report").length >= 1);
  host.notify("assign", assignmentFrom("supervisor"));
  await waitFor(() => (host.of("assign_ack").length === 1 ? true : null));

  await agent.sendFromTool({ to: "supervisor", text: "status" }); // back upstream
  await agent.sendFromTool({ to: "planner", text: "note to self" }); // this role
  await agent.sendFromTool({ to: "builder", text: "build it" });
  await assert.rejects(
    () => agent.completeFromTool({ outcome: "done" }),
    /missing handoff: 1 of 2 required distinct downstream roles/,
  );

  await agent.sendFromTool({ to: "writer", text: "document it" });
  const result = await agent.completeFromTool({ outcome: "done" });
  assert.equal(result.outcome, "done");
  assert.equal(completions(host).length, 1);
});

test("force needs a reason, and the reason it takes is stamped into the head", async () => {
  const { agent, host } = await startAgent({ relay: { required: ["writer"] } });
  agent.start();
  await waitFor(() => host.of("report").length >= 1);

  await assert.rejects(
    () =>
      agent.completeFromTool({
        outcome: "done",
        text: "outline is done",
        force: true,
        reason: "   ",
      }),
    /missing handoff to: writer/,
  );
  assert.deepEqual(completions(host), []);

  const result = await agent.completeFromTool({
    outcome: "done",
    text: "outline is done",
    force: true,
    reason: "writer is offline for the day",
  });
  assert.equal(result.head, "relay-guard-forced: writer is offline for the day | outline is done");
  assert.equal(completions(host)[0].data.head, result.head);
});

test("a refusal detaches nothing, and the same call lands once the handoff is out", async () => {
  const { agent, host, surface } = await startAgent({ relay: { required: ["writer"] } });
  agent.start();
  await waitFor(() => host.of("report").length >= 1);

  await assert.rejects(() => agent.completeFromTool({ outcome: "done", text: "half done" }), /relay guard/);
  assert.equal(agent.status().connected, true);
  assert.equal(agent.status().stats.completions, 0);
  assert.deepEqual(surface.calls.exits, []);
  assert.deepEqual(host.of("detach"), []);
  assert.deepEqual(completions(host), []);

  await agent.sendFromTool({ to: "writer", text: "the outline so far" });
  await agent.completeFromTool({ outcome: "done", text: "half done" });
  assert.equal(agent.status().stats.completions, 1);
  assert.deepEqual(surface.calls.exits, ["done"]);
});

test("a send the client refused is not a handoff", async () => {
  const { agent, host } = await startAgent({ relay: { required: ["writer"] }, failSend: true });
  agent.start();
  await waitFor(() => host.of("report").length >= 1);

  await assert.rejects(() => agent.sendFromTool({ to: "writer", text: "outline" }), /no_route/);
  await assert.rejects(() => agent.completeFromTool({ outcome: "done" }), /missing handoff to: writer/);
});

test("the handoff ledger belongs to the session, not to one task", async () => {
  const { agent, host } = await startAgent({ relay: { required: ["writer"] } });
  agent.start();
  await waitFor(() => host.of("report").length >= 1);

  // A note sent before the assignment arrived is still this session reaching
  // the role the policy names.
  await agent.sendFromTool({ to: "writer", text: "preamble" });
  host.notify("assign", assignArgs());
  await waitFor(() => (host.of("assign_ack").length === 1 ? true : null));

  const result = await agent.completeFromTool({ outcome: "done", text: "wrote the notes" });
  assert.equal(result.outcome, "done");
  assert.equal(completions(host).length, 1);
});

