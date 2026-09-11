// Protocol vocabulary tests. Every assertion about a frame's shape is anchored
// either on a wire vector the Rust encoder produced or on the reducer rules in
// `crates/onlyne-session/src/lifecycle.rs`.

import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

import {
  IMAGE_DATA_MAX_BYTES,
  MAX_HEAD_CHARS,
  PROTOCOL_VERSION,
  SEQ_BASE,
  completeReport,
  headOf,
  heartbeatReport,
  helloArgs,
  imagePart,
  injectionText,
  normalizeOutcome,
  observationFor,
  readPluginVersion,
  readyReport,
  sendEnvelope,
  stdinTaskText,
  welcomeFrom,
} from "./protocol.mjs";

const VECTOR_DIR = fileURLToPath(new URL("../../../crates/onlyne-proto/tests/wire_vectors/", import.meta.url));
const hasVectors = existsSync(VECTOR_DIR);
const vectorFrame = (name) => JSON.parse(JSON.parse(readFileSync(`${VECTOR_DIR}${name}`, "utf8")).frame);
const ASSIGN = () => vectorFrame("adapter_host_assign.json");
const packageJson = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8"));

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

test("hello carries the package version and a flat agent mount", () => {
  assert.equal(readPluginVersion(), packageJson.version);
  const args = helloArgs({
    role: "planner",
    session: "8b1c",
    taskId: "11111111-1111-4111-8111-111111111111",
    pid: 4212,
    capabilities: ["register", "report", "inject", "recycle"],
  });
  assert.deepEqual(args, {
    protocol: PROTOCOL_VERSION,
    plugin: "pi-onlyne",
    version: packageJson.version,
    kind: "agent",
    capabilities: ["register", "report", "inject", "recycle"],
    mount: {
      role: "planner",
      session: "8b1c",
      task_id: "11111111-1111-4111-8111-111111111111",
      pid: 4212,
    },
  });
});

test("hello names exactly the keys the host's own vector names", { skip: !hasVectors }, () => {
  const reference = vectorFrame("adapter_plugin_hello_plan_example.json");
  const ours = { op: "hello", args: helloArgs({ role: "planner", session: "8b1c", capabilities: ["register"] }) };
  assert.deepEqual(Object.keys(ours.args).sort(), Object.keys(reference.args).sort());
  assert.deepEqual(Object.keys(ours.args.mount).sort(), ["role", "session"]);
  assert.equal(reference.args.mount.role, ours.args.mount.role);
  assert.equal(ours.args.kind, reference.args.kind);
  assert.equal(ours.args.protocol, reference.args.protocol);
});

test("welcome parses the host's response body and rejects junk", () => {
  const args = {
    protocol: 1,
    role: "planner",
    session_id: "s1",
    generation: 1,
    prose: "Read the incoming task",
    server: { connected: true, cluster: "local", name: "server" },
    host_capabilities: ["probe", "recycle"],
  };
  const welcome = welcomeFrom({ op: "welcome", args });
  assert.deepEqual(welcome, {
    protocol: 1,
    role: "planner",
    sessionId: "s1",
    generation: 1,
    prose: "Read the incoming task",
    server: { connected: true, cluster: "local", name: "server" },
    hostCapabilities: ["probe", "recycle"],
  });
  assert.deepEqual(welcomeFrom(args)?.role, "planner");
  assert.equal(welcomeFrom(null), null);
  assert.equal(welcomeFrom({ op: "welcome", args: { prose: "x" } }), null);
  assert.deepEqual(welcomeFrom({ op: "welcome", args: { role: "builder" } }), {
    protocol: 1,
    role: "builder",
    sessionId: null,
    generation: 1,
    prose: "",
    server: null,
    hostCapabilities: [],
  });
});

test("ready matches the host's ready vector, which relays a cluster this plugin never speaks for", { skip: !hasVectors }, () => {
  const reference = vectorFrame("adapter_plugin_report.json");
  assert.equal(reference.op, "report");
  assert.equal(reference.args.kind, "ready");
  const { cluster_ref: relayed, ...local } = reference.args.data;
  assert.equal(relayed, "cluster-b");
  const ours = readyReport({ taskId: local.task_id, sessionId: local.session_id, generation: local.generation, seq: local.seq });
  assert.deepEqual(ours, { kind: "ready", data: local });
});
test("heartbeat carries a full legal observation", () => {
  const report = heartbeatReport({ taskId: "t1", generation: 2, seq: SEQ_BASE + 7, agent: "running" });
  assert.equal(report.kind, "heartbeat");
  assert.equal(report.data.task_id, "t1");
  assert.equal(report.data.seq, SEQ_BASE + 7);
  assert.deepEqual(report.data.observed, {
    version: { generation: 2, seq: SEQ_BASE + 7 },
    generation_live: true,
    isolate_after: 1,
    terminate_after: 3,
    mismatch_count: 0,
    agent: "running",
    delivery: "none",
    resource: "attached",
    recovery: "none",
    outcome: "pending",
    public: "working",
  });
});

test("every observation is a state tuple the reducer calls legal", () => {
  // Mirrors onlyne-session's `project`: running works, idle/ready wait, booting creates.
  const expected = { booting: "created", ready: "idle", running: "working", idle: "idle", gone: "created" };
  for (const [agent, projected] of Object.entries(expected)) {
    const observed = observationFor(agent, { generation: 1, seq: 1 });
    assert.equal(observed.public, projected, `agent=${agent}`);
    assert.notEqual(observed.isolate_after, 0);
    assert.notEqual(observed.terminate_after, 0);
    assert.equal(observed.outcome, "pending");
    assert.equal(observed.delivery, "none");
    assert.equal(observed.recovery, "none");
  }
});

test("a completion names an outcome and a single-line head", () => {
  assert.deepEqual(completeReport({ taskId: "t1", outcome: "failed", head: "broke\non line two" }), {
    kind: "complete",
    data: { task_id: "t1", outcome: "failed", head: "broke on line two" },
  });
  // No head key at all when there is nothing to summarise.
  assert.deepEqual(completeReport({ taskId: "t1", outcome: "done", head: "   " }), {
    kind: "complete",
    data: { task_id: "t1", outcome: "done" },
  });
  assert.equal(completeReport({ taskId: "t1", outcome: "weird" }).data.outcome, "done");
  assert.equal(normalizeOutcome(undefined), "done");
  assert.equal(normalizeOutcome("cancelled"), "cancelled");
});

test("the ledger head is capped at the plan's 200 characters", () => {
  assert.equal(MAX_HEAD_CHARS, 200);
  assert.equal(headOf("x".repeat(500)).length, 200);
  assert.equal(headOf("  spaced   out \n text "), "spaced out text");
  assert.equal(headOf(undefined), "");
});

test("an assignment becomes one message naming its origin and payload", { skip: !hasVectors }, () => {
  const assign = ASSIGN().args;
  const text = injectionText({ assign, proseIsNew: true });
  assert.match(text, /^\[onlyne\] task 11111111-1111-4111-8111-111111111111 from role:planner \(kind task\)/);
  assert.match(text, /\[onlyne\] role prose from the spec:\nRead the incoming task/);
  assert.match(text, /\nbuild it\n?$/);

  const repeat = injectionText({ assign, proseIsNew: false });
  assert.doesNotMatch(repeat, /role prose from the spec/);
  assert.match(repeat, /build it/);
});

test("an empty-bodied assignment still produces an instruction", () => {
  const text = injectionText({
    assign: { task_id: "t9", envelope: { id: "e9", kind: "note", from: { gateway: { gateway: "fg1", channel: "fake", conversation: "c1" } }, body: {} } },
    proseIsNew: false,
    attachmentPaths: ["/ws/.onlyne/tmp/attachments/a.png"],
  });
  assert.match(text, /from gateway:fg1:fake:c1/);
  assert.match(text, /the task carried no text/);
  assert.match(text, /\/ws\/\.onlyne\/tmp\/attachments\/a\.png/);
});

test("a note carries no idempotency key and a task carries both", { skip: !hasVectors }, () => {
  const note = sendEnvelope({ from: "planner", to: "builder", kind: "note", text: "ping" });
  assert.deepEqual(Object.keys(note).sort(), ["admin", "body", "from", "id", "kind", "protocol", "to", "ts"].sort());
  assert.deepEqual(
    Object.keys(note).sort(),
    Object.keys(vectorFrame("adapter_plugin_send_note.json").args).sort(),
  );
  assert.match(note.id, UUID_RE);
  assert.deepEqual(note.from, { role: { role: "planner" } });

  const task = sendEnvelope({ from: "planner", to: "builder", kind: "task", text: "build it" });
  assert.match(task.op_id, /^o-[0-9a-f-]{36}$/);
  assert.match(task.causality.task, UUID_RE);
  assert.equal(task.causality.hop, 0);
  assert.equal(task.causality.attempt, 0);
  assert.throws(() => sendEnvelope({ from: "planner", to: "builder" }), /body requires text or image/);
});

test("images are accepted only in the four core mimes and under the byte ceiling", () => {
  const part = imagePart({ data: Buffer.from([1, 2, 3]), mime: "image/png", name: "shot.png" });
  assert.equal(part.data_base64, Buffer.from([1, 2, 3]).toString("base64"));
  assert.equal(part.mime, "image/png");
  assert.equal(part.name, "shot.png");
  assert.throws(() => imagePart({ data: Buffer.from([1]), mime: "image/svg+xml" }), /unsupported/);
  assert.throws(
    () => imagePart({ data: Buffer.alloc(IMAGE_DATA_MAX_BYTES + 1), mime: "image/png" }),
    /exceeds/,
  );
});

test("the stdin route recognises only the config_get overload", () => {
  assert.deepEqual(stdinTaskText({ key: "stdin:do the thing" }), { text: "do the thing" });
  assert.equal(stdinTaskText({ key: "model.name" }), null);
  assert.equal(stdinTaskText({}), null);
  assert.equal(stdinTaskText(undefined), null);
});
