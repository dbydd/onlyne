// Integration: one pi session mounted on two clusters at once.
//
// The unit tests prove the routing rule; this case proves the wiring the rule
// is plugged into — the switch file, the two registrations in one runtime
// directory, two live sockets, one pi surface — because that path only exists
// as a whole (`docs/v2.12-ONE-SESSION-TWO-CLUSTERS.md`).
//
// It drives the real factory with the fake pi host `resident-claim.test.mjs`
// uses, and answers each socket with a hand-written welcome. What is under test
// is the plugin: which socket each mount dials, what the model sees, and where
// a reply lands. The clients themselves are not started, so the case needs no
// server and no built binary.
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { registerHooks } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, test } from "node:test";

import { createFrameDecoder, encodeFrame } from "./frame.mjs";
import { registrationPath, socketPath } from "./socket.mjs";

/**
 * The two modules `index.ts` imports for its own sake, as a running extension
 * sees them: `defineTool` is the identity and `Type` only has to build
 * something. The pi runtime itself is the fake below.
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

const ROLE = "lazuli";
const ALPHA = "alpha";
const BETA = "beta";
const SESSION_ID = "b1f0c0de-6a1e-4b0a-9d3f-2c7a5e4190ab";
const TASK_ALPHA = "11111111-1111-4111-8111-111111111111";
const TASK_BETA = "22222222-2222-4222-8222-222222222222";

/** Names that would make this process look like a spawned session. */
const IDENTITY_ENV = ["ONLYNE_ROLE", "ONLYNE_SESSION_ID", "ONLYNE_TASK_ID", "ONLYNE_WORKSPACE", "ONLYNE_RESIDENT", "ONLYNE_SOCKET", "ONLYNE_RUNTIME_DIR"];

const cleanups = [];
afterEach(async () => {
  while (cleanups.length > 0) await cleanups.pop()();
});

const settle = (ms = 150) => new Promise((resolve) => setTimeout(resolve, ms));

async function waitFor(predicate, { timeoutMs = 3_000, stepMs = 5 } = {}) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = predicate();
    if (value) return value;
    if (Date.now() > deadline) throw new Error("timed out waiting for the expected state");
    await new Promise((resolve) => setTimeout(resolve, stepMs));
  }
}

/** One pi extension host: the handlers the factory registers, and its surface. */
function fakePi() {
  const handlers = new Map();
  const tools = [];
  const messages = [];
  return {
    handlers,
    tools,
    messages,
    on(event, handler) {
      handlers.set(event, handler);
    },
    registerTool(tool) {
      tools.push(tool);
    },
    registerCommand() {},
    sendUserMessage() {},
    sendMessage(message) {
      messages.push(message);
    },
    appendEntry() {},
  };
}

function sessionContext(cwd, sessionId) {
  return {
    cwd,
    sessionManager: { getSessionId: () => sessionId },
    isIdle: () => true,
    hasPendingMessages: () => false,
    ui: { setWidget() {}, setStatus() {}, notify() {} },
  };
}

/** The client behind one socket: it answers `hello` and records every frame. */
class FakeHost {
  constructor({ role, cluster }) {
    this.role = role;
    this.cluster = cluster;
    this.server = createServer((socket) => this.onConnection(socket));
    this.frames = [];
    this.sockets = new Set();
  }

  listen(path) {
    return new Promise((resolve) => this.server.listen(path, resolve));
  }

  onConnection(socket) {
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
              role: this.role,
              cluster: this.cluster,
              session_id: frame.args?.mount?.session ?? null,
              generation: 1,
              prose: `role ${this.role} in ${this.cluster}`,
            },
          }
        : null;
    socket.write(encodeFrame({ reply_to: frame.id, ok: true, data }));
  }

  /** Push one frame to the plugin, the way a client sends an assign. */
  notify(op, args) {
    for (const socket of this.sockets) socket.write(encodeFrame({ op, args }));
  }

  get hellos() {
    return this.frames.filter((frame) => frame.op === "hello");
  }

  op(name) {
    return this.frames.filter((frame) => frame.op === name);
  }

  close() {
    for (const socket of this.sockets) socket.destroy();
    return new Promise((resolve) => this.server.close(resolve));
  }
}

/**
 * One cluster's half of the setup: a role workspace, the registration a running
 * client would have published, and the socket that registration names.
 */
function cluster(name, env) {
  const root = mkdtempSync(join(tmpdir(), `pi-onlyne-${name}-`));
  const host = new FakeHost({ role: ROLE, cluster: name });
  const socket = socketPath(root, env);
  writeFileSync(
    registrationPath(root, env),
    JSON.stringify({
      kind: "client",
      role: ROLE,
      cluster: name,
      root,
      pid: process.pid,
      version: "2.4.0",
      runtime: "pi",
      placement: "external",
    })
  );
  cleanups.push(async () => {
    await host.close();
    rmSync(root, { recursive: true, force: true });
  });
  return { name, root, host, socket };
}

/** The whole process environment this case needs, and its own back after. */
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

/** The delivery shape the client sends, as `agent.test.mjs` carries it. */
const assignArgs = (taskId, text) => ({
  envelope: {
    protocol: 1,
    id: taskId === TASK_ALPHA ? "3f2a1c4e-5b6d-4e7f-8a90-1b2c3d4e5f60" : "4a3b2c1d-6e7f-4a90-8b1c-2d3e4f506172",
    kind: "task",
    from: { role: { role: "dpdp" } },
    to: { role: { role: ROLE } },
    causality: { task: taskId, hop: 0, attempt: 0 },
    body: { text },
  },
  text: `From dpdp:\n\n${text}`,
  task_id: taskId,
  generation: 1,
  scope: "role",
});

/** Boot the factory over a body whose switch file names both clusters. */
async function bootTwoMounts() {
  const runtime = mkdtempSync(join(tmpdir(), "pi-onlyne-mounts-runtime-"));
  cleanups.push(() => rmSync(runtime, { recursive: true, force: true }));
  const env = { ONLYNE_RUNTIME_DIR: runtime };
  withEnv(env);

  const alpha = cluster(ALPHA, env);
  const beta = cluster(BETA, env);
  await alpha.host.listen(alpha.socket);
  await beta.host.listen(beta.socket);

  const body = mkdtempSync(join(tmpdir(), "pi-onlyne-mounts-body-"));
  cleanups.push(() => rmSync(body, { recursive: true, force: true }));
  mkdirSync(join(body, ".pi"), { recursive: true });
  writeFileSync(
    join(body, ".pi", "onlyne.json"),
    JSON.stringify({
      mounts: [
        { role: ROLE, cluster: ALPHA, workspace: alpha.root },
        { role: ROLE, cluster: BETA, workspace: beta.root },
      ],
    })
  );

  const pi = fakePi();
  onlyne(pi);
  cleanups.push(async () => pi.handlers.get("session_shutdown")({ reason: "quit" }));
  await pi.handlers.get("session_start")({ reason: "startup" }, sessionContext(body, SESSION_ID));
  return { pi, body, alpha, beta };
}

const toolOf = (pi, name) => pi.tools.find((tool) => tool.name === name);
const ctxOf = (boot) => sessionContext(boot.body, SESSION_ID);

test("one session mounts both clusters and each delivery names its layer", async () => {
  const boot = await bootTwoMounts();

  // Two sockets, two hellos, one session id: the mounts are the same session on
  // two servers, and each hello names the role its own client registered.
  await waitFor(() => boot.alpha.host.hellos.length === 1 && boot.beta.host.hellos.length === 1);
  assert.equal(boot.alpha.host.hellos[0].args.mount.role, ROLE);
  assert.equal(boot.beta.host.hellos[0].args.mount.role, ROLE);
  assert.equal(boot.alpha.host.hellos[0].args.mount.session, SESSION_ID);
  assert.equal(boot.beta.host.hellos[0].args.mount.session, SESSION_ID);
  // A taskless resident reports no ready of its own, so the handshake's own
  // completion is what the delivery waits on: both sockets have answered by now.
  await settle();

  boot.alpha.host.frames.length = 0;
  boot.beta.host.frames.length = 0;
  boot.pi.messages.length = 0;

  // A task from each layer reaches the one session, and the card says which.
  boot.alpha.host.notify("assign", assignArgs(TASK_ALPHA, "work for the centre"));
  boot.beta.host.notify("assign", assignArgs(TASK_BETA, "work for the house"));
  await waitFor(() => boot.pi.messages.length === 2);

  const cards = boot.pi.messages.map((message) => message.details);
  assert.deepEqual(cards.map((card) => card.cluster).sort(), [ALPHA, BETA]);
  assert.deepEqual(cards.map((card) => card.taskId).sort(), [TASK_ALPHA, TASK_BETA]);

  // With both layers holding work, a bare role cannot be resolved and the
  // refusal names both mounts rather than picking one.
  const send = toolOf(boot.pi, "onlyne_send");
  await assert.rejects(
    () => send.execute("t1", { to: "worker", text: "hello" }, null, null, ctxOf(boot)),
    (error) => /more than one mount has work in flight/.test(error.message) && error.message.includes(ALPHA) && error.message.includes(BETA)
  );

  // An explicit cluster addresses only that layer's client.
  boot.alpha.host.frames.length = 0;
  boot.beta.host.frames.length = 0;
  const sent = await send.execute("t2", { to: `${ALPHA}:worker`, text: "hello" }, null, null, ctxOf(boot));
  assert.deepEqual(sent.details, { to: "worker" });
  assert.equal(boot.alpha.host.op("send").length, 1);
  assert.equal(boot.beta.host.op("send").length, 0);
  assert.equal(boot.alpha.host.op("send")[0].args.to.role.role, "worker");

  // Each completion settles its own ledger: the same session, two answer paths.
  const complete = toolOf(boot.pi, "onlyne_complete");
  await complete.execute("c1", { outcome: "done", summary: "centre done", cluster: ALPHA }, null, null, ctxOf(boot));
  await complete.execute("c2", { outcome: "done", summary: "house done", cluster: BETA }, null, null, ctxOf(boot));
  const alphaComplete = boot.alpha.host.op("report").filter((frame) => frame.args.kind === "complete");
  const betaComplete = boot.beta.host.op("report").filter((frame) => frame.args.kind === "complete");
  assert.deepEqual(alphaComplete.map((frame) => frame.args.data.task_id), [TASK_ALPHA]);
  assert.deepEqual(betaComplete.map((frame) => frame.args.data.task_id), [TASK_BETA]);
});

test("a completion with no cluster answers the one mount holding the task", async () => {
  const boot = await bootTwoMounts();
  await waitFor(() => boot.alpha.host.hellos.length === 1 && boot.beta.host.hellos.length === 1);
  await settle();
  boot.alpha.host.frames.length = 0;
  boot.beta.host.frames.length = 0;

  // Only the house layer handed work over, so the reply has exactly one home.
  boot.beta.host.notify("assign", assignArgs(TASK_BETA, "one task"));
  await waitFor(() => boot.pi.messages.length === 1);

  const complete = toolOf(boot.pi, "onlyne_complete");
  await complete.execute("c1", { outcome: "done", summary: "done" }, null, null, ctxOf(boot));
  assert.deepEqual(
    boot.beta.host.op("report").filter((frame) => frame.args.kind === "complete").map((frame) => frame.args.data.task_id),
    [TASK_BETA]
  );
  assert.deepEqual(boot.alpha.host.op("report").filter((frame) => frame.args.kind === "complete"), []);
});
