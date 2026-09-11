// Agent state-machine tests against a fake host on a real unix socket: the same
// framing, the same frames, no Rust process. The assign frame is the host's own
// wire vector, so the input side is byte-identical to what the client sends.

import assert from "node:assert/strict";
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, test } from "node:test";

import { PANE_CLAIMS_RELATIVE_DIR, paneClaimFileName } from "./attribution.mjs";
import { OnlyneAgent } from "./agent.mjs";
import { createFrameDecoder, encodeFrame } from "./frame.mjs";
import { SEQ_BASE, readyReport } from "./protocol.mjs";

const VECTOR_DIR = fileURLToPath(new URL("../../../crates/onlyne-proto/tests/wire_vectors/", import.meta.url));
const ASSIGN_FRAME = existsSync(VECTOR_DIR)
  ? JSON.parse(JSON.parse(readFileSync(`${VECTOR_DIR}adapter_host_assign.json`, "utf8")).frame)
  : null;
const TASK_ID = "11111111-1111-4111-8111-111111111111";
const SESSION_ID = "8b1c";

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
 * session does when both frames are ready at once.
 */
class FakeHost {
  constructor({ coalesceAssign = null } = {}) {
    this.server = createServer((socket) => this.onConnection(socket));
    this.frames = [];
    this.sockets = new Set();
    this.connections = 0;
    this.coalesceAssign = coalesceAssign;
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
    const reply = encodeFrame({ reply_to: frame.id, ...body });
    const push = frame.op === "hello" && this.coalesceAssign
      ? encodeFrame({ op: "assign", args: this.coalesceAssign })
      : null;
    socket.write(push ? Buffer.concat([reply, push]) : reply);
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
async function startAgent({ surface = fakeSurface(), capabilities, options = {}, coalesceAssign = null } = {}) {
  const dir = mkdtempSync(join(tmpdir(), "pi-onlyne-agent-"));
  const socketPath = join(dir, "s");
  const host = new FakeHost({ coalesceAssign });
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
    ...(options.claimStore ? { claimStore: options.claimStore } : {}),
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

test("the pane claim follows the session: mount, assign, bye, stop", async () => {
  const published = [];
  const claimStore = { publish: (claim) => published.push(claim) };
  const { agent, host } = await startAgent({ options: { claimStore } });
  const paneKey = "45e603f7-0772-48aa-bcf6-832272747713:b6d067b6-9255-4f5c-a13f-24f194ea0560";

  await inOrcaPane({ ORCA_PANE_KEY: paneKey, ORCA_TERMINAL_HANDLE: "term_1" }, async () => {
    agent.start();
    // Mount: the pane binding is stated as soon as the welcome is adopted.
    const [mounted] = await waitFor(() => (published.length === 1 ? published : null));
    assert.deepEqual(mounted, {
      pane_key: paneKey,
      tab_id: "45e603f7-0772-48aa-bcf6-832272747713",
      leaf_id: "b6d067b6-9255-4f5c-a13f-24f194ea0560",
      handle: "term_1",
      worktree_id: null,
      role: "planner",
      task_id: TASK_ID,
    });

    // A task named after the mount refreshes the claim rather than leaving a
    // stale task id in it.
    host.notify("assign", { ...assignArgs(), task_id: "22222222-2222-4222-8222-222222222222" });
    await waitFor(() => (published.length === 2 ? true : null));
    assert.equal(published[1].task_id, "22222222-2222-4222-8222-222222222222");
    assert.equal(published[1].pane_key, paneKey, "the pane is what the claim is for");

    // The host ended the session while pi stays up: the claim goes with it.
    host.notify("bye", { reason: "session ended" });
    await waitFor(() => (published.length === 3 ? true : null));
    assert.equal(published[2], null);

    agent.stop("test");
    assert.equal(published.at(-1), null);
  });
});

test("the claim lands in this pane's own file, and a dropped socket keeps it", async () => {
  const { agent, host, dir } = await startAgent();
  const paneKey = "45e603f7-0772-48aa-bcf6-832272747713:b6d067b6-9255-4f5c-a13f-24f194ea0560";
  const claimPath = join(dir, PANE_CLAIMS_RELATIVE_DIR, paneClaimFileName(paneKey));

  await inOrcaPane({ ORCA_PANE_KEY: paneKey, ORCA_TERMINAL_HANDLE: "term_1" }, async () => {
    agent.start();
    // Mount: the claim is a file of its own, named after this pane.
    await waitFor(() => (existsSync(claimPath) ? true : null));
    const mounted = JSON.parse(readFileSync(claimPath, "utf8"));
    assert.equal(mounted.pane_key, paneKey);
    assert.equal(mounted.handle, "term_1");
    assert.equal(mounted.role, "planner");
    assert.match(mounted.updated_at, /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/);

    // An assignment refreshes the claim in place: still one file for the pane.
    const reassigned = "22222222-2222-4222-8222-222222222222";
    host.notify("assign", { ...assignArgs(), task_id: reassigned });
    await waitFor(() => (JSON.parse(readFileSync(claimPath, "utf8")).task_id === reassigned ? true : null));
    assert.deepEqual(readdirSync(join(dir, PANE_CLAIMS_RELATIVE_DIR)), [paneClaimFileName(paneKey)]);

    // The socket dying is not the session ending: pi stays up and may be handed
    // another task, so the claim stays. The host is gone for good here, so
    // nothing can put the file back and the assertion is a real one.
    await host.close();
    await waitFor(() => (agent.connected === false ? true : null));
    assert.equal(existsSync(claimPath), true, "a dropped socket does not clear the claim");
  });
});

test("stop removes this pane's claim file", async () => {
  const { agent, host, dir } = await startAgent();
  const paneKey = "45e603f7-0772-48aa-bcf6-832272747713:b6d067b6-9255-4f5c-a13f-24f194ea0560";
  const claimPath = join(dir, PANE_CLAIMS_RELATIVE_DIR, paneClaimFileName(paneKey));

  await inOrcaPane({ ORCA_PANE_KEY: paneKey }, async () => {
    agent.start();
    await waitFor(() => (existsSync(claimPath) ? true : null));

    agent.stop("test");
    assert.equal(existsSync(claimPath), false, "the plugin left, so its claim went with it");
  });
});

test("outside an Orca pane no claim is ever published", async () => {
  const published = [];
  const { agent } = await startAgent({ options: { claimStore: { publish: (claim) => published.push(claim) } } });
  const saved = process.env.ORCA_PANE_KEY;
  delete process.env.ORCA_PANE_KEY;
  try {
    agent.start();
    await waitFor(() => (published.length === 1 ? true : null));
    assert.deepEqual(published, [null], "a plain pi session claims nothing");
  } finally {
    if (saved !== undefined) process.env.ORCA_PANE_KEY = saved;
  }
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

  await waitFor(() => (host.connections >= 2 ? true : null));
  const complete = await waitFor(() => host.of("report").find((report) => report.kind === "complete"));
  assert.equal(complete.data.head, "done offline");
  assert.equal(agent.status().connected, true, "the plugin re-hellos after a disconnect");
  const hellos = host.of("hello");
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

