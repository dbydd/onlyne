// One process serves one resident, and a subagent is never it.
//
// Two readings decide the resident branch of `session_start`, and these tests
// drive both through the real factory: the host's own declaration that the
// session is a subagent (`ctx.agent.kind`, omp), and — on a host that declares
// nothing (`@earendil-works/pi-coding-agent` 1.3.0) — the process-wide claim
// (`docs/v2.11-LIFECYCLE-IS-DECLARED-NOT-INFERRED.md` §4.2).
//
// The live defect the claim pins (`caerulea-main` mislabelled `exited`,
// 2026-10-19): a resident pi is started by hand, not spawned by the client, so
// a `task` subagent's sidechain inside that same process inherits an
// environment with no task id in it, resolves the same cwd config and reaches
// the resident branch through a second call of this factory with fresh closure
// state. `boundSessionId` was `null` there, so a guard written as
// `boundSessionId !== null && ...` let the child open a connection that
// impersonated the parent's resident alias; the child's own `detach` then
// retired the session the parent was still serving.
//
// The socket is real and the fake client behind it is what proves a mount: a
// session that mounts registers tools and opens one connection, a session that
// is refused does neither. The claim is only observable through behaviour, so
// every step that reads it starts the next session in the same process and
// asserts what happened to that one.
import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { registerHooks } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, test } from "node:test";

import { createFrameDecoder, encodeFrame } from "./frame.mjs";

/**
 * The two modules `index.ts` imports for its own sake, as a running extension
 * sees them: `defineTool` is the identity (the typebox schema is what pi reads,
 * and no tool is invoked here) and `Type` only has to build something. The pi
 * runtime itself is the fake below; a test must not need the installed SDK.
 */
const PI_MODULES = {
  "@earendil-works/pi-coding-agent": "export const defineTool = (tool) => tool;",
  typebox: [
    "const shape = () => ({});",
    "export const Type = { Object: shape, String: shape, Optional: shape, Array: shape };",
  ].join("\n"),
};

registerHooks({
  resolve(specifier, context, nextResolve) {
    const source = PI_MODULES[specifier];
    if (source) return { url: `data:text/javascript,${encodeURIComponent(source)}`, shortCircuit: true };
    return nextResolve(specifier, context);
  },
});

const { default: onlyne } = await import("./index.ts");

/** The role this process serves, as the resident config names it. */
const ROLE = "resident-role";
const SESSION_A = "b1f0c0de-6a1e-4b0a-9d3f-2c7a5e4190ab";
const SESSION_B = "c2a1d1ef-7b2f-4c1b-8e40-3d8b6f52a1bc";
const SESSION_C = "d3b2e2f0-8c30-4d2c-9f51-4e9c7a63b2cd";
const SESSION_D = "e4c3f301-9d41-4e3d-a062-5fad8b74c3de";
const TASK_ID = "1f2e3d4c-5b6a-4978-8899-aabbccddeeff";

/** Every tool the module registers, in registration order. */
const TOOLS = ["onlyne_send", "onlyne_complete", "onlyne_handoff"];

/** The agent identity a host declares for a `task` child, as omp reports one. */
const CHILD_AGENT = Object.freeze({ kind: "sub", id: "0-Explore", name: "explore", depth: 1, parentId: "Main" });

/**
 * The `ONLYNE_*` names that decide which branch the plugin takes. A test may
 * itself run under an onlyne client, whose injected session id and task id would
 * make every mount here a spawned one, so all of them are replaced.
 */
const IDENTITY_ENV = ["ONLYNE_ROLE", "ONLYNE_SESSION_ID", "ONLYNE_TASK_ID", "ONLYNE_WORKSPACE", "ONLYNE_RESIDENT"];

/** Work the afterEach hook undoes, whichever way a test ends. */
const cleanups = [];
afterEach(async () => {
  while (cleanups.length > 0) await cleanups.pop()();
});

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
 * Give a connection the chance to appear that a mount would have given it: the
 * connect and the host's accept are each one turn of the loop away, the window
 * the agent tests read the same absence in.
 */
const settle = () => new Promise((resolve) => setTimeout(resolve, 120));

/** One pi extension host: the handlers the factory registers, and its surface. */
function fakePi() {
  const handlers = new Map();
  const tools = [];
  const commands = [];
  return {
    handlers,
    tools,
    commands,
    on(event, handler) {
      handlers.set(event, handler);
    },
    registerTool(tool) {
      tools.push(tool);
    },
    registerCommand(name, command) {
      commands.push({ name, command });
    },
    sendUserMessage() {},
    sendMessage() {},
    appendEntry() {},
  };
}

/**
 * The context pi hands a handler: the workspace, this session's id, its pane,
 * and — where the host declares one — the agent the session runs.
 */
function sessionContext(cwd, sessionId, agent = null) {
  return {
    cwd,
    // pi names the session it is starting; `null` is an older pi that cannot.
    sessionManager: sessionId === null ? {} : { getSessionId: () => sessionId },
    isIdle: () => true,
    hasPendingMessages: () => false,
    ui: { setWidget() {}, setStatus() {}, notify() {} },
    ...(agent ? { agent } : {}),
  };
}

/**
 * The client side of the socket: it counts connections and answers every request
 * with the welcome a real host sends, so a mounted session reaches `connected`
 * and holds its single connection open for the rest of the test.
 */
class FakeHost {
  constructor() {
    this.server = createServer((socket) => this.onConnection(socket));
    this.connections = 0;
    this.sockets = new Set();
    this.frames = [];
  }

  listen(socketPath) {
    return new Promise((resolve) => this.server.listen(socketPath, resolve));
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
    this.frames.push(frame);
    if (frame.id === undefined) return;
    const data =
      frame.op === "hello"
        ? {
            op: "welcome",
            args: {
              protocol: 1,
              role: ROLE,
              session_id: frame.args?.mount?.session ?? null,
              generation: 1,
            },
          }
        : null;
    socket.write(encodeFrame({ reply_to: frame.id, ok: true, data }));
  }

  /** How many sessions this process opened a connection for. */
  get hellos() {
    return this.frames.filter((frame) => frame.op === "hello").length;
  }

  close() {
    for (const socket of this.sockets) socket.destroy();
    return new Promise((resolve) => this.server.close(resolve));
  }
}

/**
 * A workspace, the socket its resident dials, and the fake client behind it.
 * All three go away with the test.
 */
function residentWorkspace() {
  const host = new FakeHost();
  const cwd = mkdtempSync(join(tmpdir(), "pi-onlyne-resident-"));
  const socketPath = join(cwd, "s");
  cleanups.push(async () => {
    await host.close();
    rmSync(cwd, { recursive: true, force: true });
  });
  return { host, cwd, socketPath };
}

/** Put one process environment in place, and the harness's own back afterwards. */
function withEnv(values) {
  const saved = new Map(IDENTITY_ENV.map((key) => [key, process.env[key]]));
  for (const key of IDENTITY_ENV) delete process.env[key];
  for (const [key, value] of Object.entries(values)) process.env[key] = value;
  cleanups.push(() => {
    for (const [key, value] of saved) {
      if (value === undefined) delete process.env[key];
      else process.env[key] = value;
    }
  });
}

/**
 * A resident process: a role and the injected socket path, with no task id —
 * which is exactly what a `task` child of that process inherits.
 */
function residentEnv(socketPath) {
  withEnv({ ONLYNE_ROLE: ROLE, ONLYNE_SOCKET: socketPath });
}

/** A client-spawned process: the environment the client injects. */
function spawnedEnv(socketPath) {
  withEnv({
    ONLYNE_ROLE: ROLE,
    ONLYNE_SESSION_ID: SESSION_A,
    ONLYNE_TASK_ID: TASK_ID,
    ONLYNE_SOCKET: socketPath,
  });
}

/**
 * The plugin's own log goes to `process.stderr`; a test that asserts what a
 * refusal said reads it here, and the stream is put back afterwards.
 */
function captureStderr() {
  const lines = [];
  const write = process.stderr.write;
  process.stderr.write = (chunk) => {
    lines.push(String(chunk));
    return true;
  };
  cleanups.push(() => {
    process.stderr.write = write;
  });
  return (needle) => lines.find((line) => line.includes(needle));
}

/** Shut one session down, the way pi does when its session ends. */
async function shutdownSession(pi, reason = "quit") {
  await pi.handlers.get("session_shutdown")({ reason });
}

/**
 * One extension bind for one session: a fresh factory call — exactly what pi
 * does for every session it runs in this process — then one `session_start`.
 */
async function startSession(cwd, { sessionId, reason = "startup", agent = null } = {}) {
  const pi = fakePi();
  onlyne(pi);
  cleanups.push(() => shutdownSession(pi));
  await pi.handlers.get("session_start")({ reason }, sessionContext(cwd, sessionId, agent));
  return pi;
}

/** The names of the tools a factory call registered, in order. */
const toolNames = (pi) => pi.tools.map((tool) => tool.name);

test("a session the host declares a subagent never mounts the client's session", async () => {
  const { host, cwd, socketPath } = residentWorkspace();
  await host.listen(socketPath);
  spawnedEnv(socketPath);
  const stderrLine = captureStderr();

  // A `task` child inherits the parent's whole environment, this session id
  // included, so the id cannot tell it from the session the client spawned. The
  // host's own declaration can, and it decides before either identity path.
  const child = await startSession(cwd, { sessionId: SESSION_A, agent: CHILD_AGENT });
  await settle();
  assert.deepEqual(child.tools, [], "the child registers no tool");
  assert.deepEqual(child.commands, [], "and no command");
  assert.equal(host.connections, 0, "and opens no connection");
  const refusal = stderrLine("is subagent");
  assert.ok(refusal, "the refusal is logged");
  assert.ok(refusal.includes(SESSION_A), `the refusal names the session: ${refusal}`);
  assert.ok(refusal.includes("0-Explore"), `and the subagent: ${refusal}`);
  assert.ok(refusal.includes("Main"), `and its parent: ${refusal}`);

  // The same session id, this time declared a top-level session, is the one the
  // client named — so what refused the child was the declaration, not the id.
  const named = await startSession(cwd, { sessionId: SESSION_A });
  await waitFor(() => host.connections === 1);
  assert.deepEqual(toolNames(named), TOOLS, "the named session mounts");
  await shutdownSession(named);
});

test("a host-declared subagent cannot dock onto the resident alias", async () => {
  const { host, cwd, socketPath } = residentWorkspace();
  await host.listen(socketPath);
  residentEnv(socketPath);
  const stderrLine = captureStderr();

  const resident = await startSession(cwd, { sessionId: SESSION_A });
  await waitFor(() => host.connections === 1);

  // A child of an older host that reports no session id at all: the resident
  // branch has nothing left to compare, so only the declaration refuses it.
  const child = await startSession(cwd, { sessionId: null, agent: { ...CHILD_AGENT, id: "0-Writer" } });
  await settle();
  assert.deepEqual(child.tools, [], "the child registers no tool");
  assert.equal(host.connections, 1, "and no connection of its own");
  const refusal = stderrLine("is subagent");
  assert.ok(refusal?.includes("0-Writer"), `the refusal names the child: ${refusal}`);

  // Its shutdown is not the resident's: the claim the resident holds survives
  // it, so the next session in this process is still refused.
  await shutdownSession(child);
  const afterChild = await startSession(cwd, { sessionId: SESSION_C });
  await settle();
  assert.deepEqual(afterChild.tools, [], "the child's shutdown released nothing");
  assert.equal(host.connections, 1);
  await shutdownSession(resident);
});

test("a second factory in the same process cannot mount the resident session", async () => {
  const { host, cwd, socketPath } = residentWorkspace();
  await host.listen(socketPath);
  residentEnv(socketPath);
  const stderrLine = captureStderr();

  const resident = await startSession(cwd, { sessionId: SESSION_A });
  await waitFor(() => host.connections === 1);
  assert.deepEqual(toolNames(resident), TOOLS, "the resident mounts, with its tools");

  // A host that declares no agent identity (pi) leaves the claim to refuse the
  // child: a second call of the factory with fresh closure state and another
  // session id, which is what a `task` subagent runs there.
  const sidechain = await startSession(cwd, { sessionId: SESSION_B });
  await settle();
  assert.deepEqual(sidechain.tools, [], "the child registers no tool");
  assert.deepEqual(sidechain.commands, [], "and no command");
  assert.equal(host.connections, 1, "and opens no connection");
  assert.equal(host.hellos, 1, "the one hello is the resident's");
  const refusal = stderrLine(SESSION_B);
  assert.ok(refusal, "the refusal is logged");
  assert.ok(refusal.includes(SESSION_A), `the refusal names the resident too: ${refusal}`);
  assert.ok(refusal.includes("staying inert"), refusal);

  // The child's shutdown is not the resident's: the claim it never held is still
  // held, so the next session in this process is still refused.
  await shutdownSession(sidechain);
  const afterChild = await startSession(cwd, { sessionId: SESSION_C });
  await settle();
  assert.deepEqual(afterChild.tools, [], "the child's shutdown released nothing");
  assert.equal(host.connections, 1);

  // The resident's own shutdown does release it: the next session mounts.
  await shutdownSession(resident);
  const next = await startSession(cwd, { sessionId: SESSION_D });
  await waitFor(() => host.connections === 2);
  assert.deepEqual(toolNames(next), TOOLS, "the claim is free again after the resident left");
  await shutdownSession(next);
});

test("a session replacement inside the resident process moves the claim", async () => {
  const { host, cwd, socketPath } = residentWorkspace();
  await host.listen(socketPath);
  residentEnv(socketPath);
  const stderrLine = captureStderr();

  const resident = await startSession(cwd, { sessionId: SESSION_A });
  await waitFor(() => host.connections === 1);

  // `/new`: pi replaces the live session inside this process and reports it as
  // `session_start` with another reason; the resident follows it and keeps the
  // one connection it holds.
  await resident.handlers.get("session_start")({ reason: "new" }, sessionContext(cwd, SESSION_B));
  await settle();
  assert.equal(host.connections, 1, "a replacement opens no second connection");

  // The claim travelled with the live session: the session the process left is
  // now the foreign one, and a factory that starts it stays inert.
  const replaced = await startSession(cwd, { sessionId: SESSION_A });
  await settle();
  assert.deepEqual(replaced.tools, [], "the replaced session may not claim again");
  assert.equal(host.connections, 1);
  const refusal = stderrLine("is not the resident session");
  assert.ok(refusal, "the refusal is logged");
  assert.ok(refusal.includes(SESSION_A), `the refusal names the replaced session: ${refusal}`);
  assert.ok(refusal.includes(SESSION_B), `and the live one: ${refusal}`);

  // And it is released by the mount holding it, so the process can be resident
  // for the session that comes next.
  await shutdownSession(resident);
  const next = await startSession(cwd, { sessionId: SESSION_C });
  await waitFor(() => host.connections === 2);
  await shutdownSession(next);
});

test("a host-declared replacement mounts even through a fresh factory", async () => {
  const { host, cwd, socketPath } = residentWorkspace();
  await host.listen(socketPath);
  residentEnv(socketPath);
  const stderrLine = captureStderr();

  const resident = await startSession(cwd, { sessionId: SESSION_A });
  await waitFor(() => host.connections === 1);

  // A host that re-binds the extension for `/resume` rather than following the
  // switch in place: the new session is this process's resident now, so the
  // claim moves instead of the session being refused.
  const resumed = await startSession(cwd, { sessionId: SESSION_B, reason: "resume" });
  await waitFor(() => host.connections === 2);
  assert.deepEqual(toolNames(resumed), TOOLS, "the replacing session mounts");
  assert.equal(stderrLine("is not the resident session"), undefined, "and is not refused");

  // The claim is the replacing session's: the one it replaced may not claim it
  // back, and the replacing session's own shutdown releases it.
  const replaced = await startSession(cwd, { sessionId: SESSION_A });
  await settle();
  assert.deepEqual(replaced.tools, [], "the replaced session may not claim again");
  await shutdownSession(resumed);
  const next = await startSession(cwd, { sessionId: SESSION_C });
  await waitFor(() => host.connections === 3);
  await shutdownSession(next);
});

test("an older pi that names no session id mounts, and claims nothing", async () => {
  const { host, cwd, socketPath } = residentWorkspace();
  await host.listen(socketPath);
  residentEnv(socketPath);
  captureStderr();

  // Nothing to compare, so the mount keeps the behaviour it had: the env-bound
  // session is the only candidate. It also leaves the claim empty, which is this
  // fix's one concession — a session that cannot be named cannot be held to.
  const unnamed = await startSession(cwd, { sessionId: null });
  await waitFor(() => host.connections === 1);
  assert.deepEqual(toolNames(unnamed), TOOLS, "an unnamed session still mounts");

  const named = await startSession(cwd, { sessionId: SESSION_A });
  await waitFor(() => host.connections === 2);
  assert.deepEqual(toolNames(named), TOOLS, "and a session that is named is not blocked by one that was not");
  await shutdownSession(named);
  await shutdownSession(unnamed);
});
