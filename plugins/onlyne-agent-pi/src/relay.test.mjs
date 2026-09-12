// The relay guard's policy reader and its verdict.
//
// The reader is the half that decides whether a workspace guards anything at
// all: a missing file, a malformed line or an unknown key must leave the guard
// off (or leave only the sound half of it on) instead of refusing completions
// on a policy nobody wrote. The client's injected policy outranks the file, and
// the file is the fallback a manual installation still has.

import assert from "node:assert/strict";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { afterEach, test } from "node:test";

import {
  DEFAULT_RELAY,
  RELAY_ENV_COUNT,
  RELAY_ENV_REQUIRED,
  RELAY_FILE,
  envRelay,
  loadRelay,
  parseRelay,
  relayEnabled,
  relayPath,
  relayRefusal,
} from "./relay.mjs";

const cleanups = [];
afterEach(() => {
  while (cleanups.length > 0) cleanups.pop()();
});

/** One temp directory holding a `relay.toml` body, when given one. */
function workspace(body) {
  const dir = mkdtempSync(join(tmpdir(), "pi-onlyne-relay-"));
  cleanups.push(() => rmSync(dir, { recursive: true, force: true }));
  if (body !== undefined) {
    writeFileSync(join(dir, RELAY_FILE), body);
  }
  return dir;
}

test("a missing policy file leaves the guard off", () => {
  const dir = workspace();
  const relay = loadRelay({ path: join(dir, RELAY_FILE), env: {} });
  assert.deepEqual(
    { required: relay.required, count: relay.count },
    { required: DEFAULT_RELAY.required, count: DEFAULT_RELAY.count },
  );
  assert.equal(relay.present, false);
  assert.equal(relay.source, "none");
  assert.equal(relay.warning, null);
  assert.equal(relayEnabled(relay), false);
  assert.equal(relay.path, join(dir, RELAY_FILE));
});

test("the default policy path is the one beside the plugin's package.json", () => {
  const path = relayPath();
  assert.ok(path.endsWith(`/${RELAY_FILE}`), path);
  // A generated workspace loads the vendored copy of this package, so the
  // policy has to travel inside it (`crates/onlyne-server/src/generate.rs`).
  assert.ok(existsSync(join(dirname(path), "package.json")), dirname(path));
});

test("both policy keys are honoured, comments and blank lines included", () => {
  const list = parseRelay('# which handoffs this session owes\nrelay_required = ["writer", "auditor"]\n');
  assert.deepEqual(list.required, ["writer", "auditor"]);
  assert.equal(list.count, null);
  assert.equal(list.warning, null);

  const counted = parseRelay("relay_required_count = 2 # distinct downstream roles\n");
  assert.deepEqual(counted.required, []);
  assert.equal(counted.count, 2);
  assert.equal(counted.warning, null);

  const both = parseRelay('relay_required = []\nrelay_required_count = 3\n');
  assert.deepEqual(both.required, []);
  assert.equal(both.count, 3);
});

test("a body outside the closed subset warns and keeps the default", () => {
  const unsupported = parseRelay(
    ["[relay]", 'relay_required = [', '  "writer",', "]", "relay_required_count = 0", "write = true"].join("\n"),
    "relay.toml",
  );
  assert.deepEqual(unsupported.required, []);
  assert.equal(unsupported.count, null);
  assert.match(unsupported.warning, /relay\.toml:1: not a `key = value` line/);
  assert.match(unsupported.warning, /relay\.toml:2: relay_required must be one line/);
  assert.match(unsupported.warning, /relay\.toml:4: not a `key = value` line/);
  assert.match(unsupported.warning, /relay\.toml:5: relay_required_count must be a positive integer/);
  assert.match(unsupported.warning, /relay\.toml:6: unknown key "write"/);

  // One bad line does not take the sound one with it.
  const partial = parseRelay('relay_required = ["writer"]\nrelay_required_count = two\n');
  assert.deepEqual(partial.required, ["writer"]);
  assert.equal(partial.count, null);
  assert.match(partial.warning, /relay_required_count must be a positive integer/);
});

test("a policy file on disk reaches the caller with its warnings", () => {
  const dir = workspace('relay_required = ["writer"]\nnonsense\n');
  const relay = loadRelay({ path: join(dir, RELAY_FILE), env: {} });
  assert.equal(relay.present, true);
  assert.equal(relay.source, "file");
  assert.deepEqual(relay.required, ["writer"]);
  assert.equal(relayEnabled(relay), true);
  assert.match(relay.warning, /relay\.toml:2:/);
});

test("an injected policy is the one in force, and the file is not consulted", () => {
  const dir = workspace('relay_required = ["legacy"]\n');
  const relay = loadRelay({
    path: join(dir, RELAY_FILE),
    env: { [RELAY_ENV_REQUIRED]: "writer, auditor", [RELAY_ENV_COUNT]: "2" },
  });
  assert.deepEqual(relay.required, ["writer", "auditor"]);
  assert.equal(relay.count, 2);
  assert.equal(relay.source, "env");
  assert.equal(relay.present, false, "the file is not where this policy came from");
  assert.equal(relay.path, join(dir, RELAY_FILE));
  assert.equal(relay.warning, null);
  assert.equal(relayEnabled(relay), true);
  // Both forms travel when the spec names both, and the list still decides.
  assert.match(relayRefusal(relay, ["builder", "auditor"]), /missing handoff to: writer/);
});

test("the environment spells the spec's two keys", () => {
  const listed = envRelay({ [RELAY_ENV_REQUIRED]: "writer,, auditor ," });
  assert.deepEqual(listed.required, ["writer", "auditor"]);
  assert.equal(listed.count, null);
  assert.equal(listed.specified, true);
  assert.equal(listed.warning, null);

  const counted = envRelay({ [RELAY_ENV_COUNT]: "2" });
  assert.deepEqual(counted.required, []);
  assert.equal(counted.count, 2);
  assert.equal(counted.specified, true);

  const nothing = envRelay({});
  assert.deepEqual(nothing.required, []);
  assert.equal(nothing.count, null);
  assert.equal(nothing.specified, false, "an absent pair is not a policy");
  assert.equal(nothing.warning, null);
});

test("an unparsable variable is reported and the file keeps its turn", () => {
  const dir = workspace('relay_required = ["legacy"]\n');
  const relay = loadRelay({
    path: join(dir, RELAY_FILE),
    env: { [RELAY_ENV_COUNT]: "two" },
  });
  assert.equal(relay.source, "file");
  assert.equal(relay.present, true);
  assert.deepEqual(relay.required, ["legacy"]);
  assert.equal(relay.count, null);
  assert.equal(relayEnabled(relay), true);
  assert.match(relay.warning, /ONLYNE_RELAY_COUNT must be a positive integer, got "two"/);

  // A variable that names no role is not a policy either; with no file behind
  // it the guard stays off, and the reason it is off is reported.
  const off = loadRelay({ path: join(workspace(), RELAY_FILE), env: { [RELAY_ENV_REQUIRED]: ", ," } });
  assert.deepEqual(off.required, []);
  assert.equal(off.count, null);
  assert.equal(off.source, "none");
  assert.equal(off.present, false);
  assert.equal(relayEnabled(off), false);
  assert.match(off.warning, /ONLYNE_RELAY_REQUIRED: no role names in ", ,"/);
});

test("the verdict names every missing edge, and the way out", () => {
  const refusal = relayRefusal({ required: ["writer", "auditor"] }, ["auditor"]);
  assert.match(refusal, /^relay guard: missing handoff to: writer \(/);
  assert.match(refusal, /force:true and a non-empty reason/);
  assert.match(refusal, /"relay-guard-forced: <reason>"/);
  assert.equal(relayRefusal({ required: ["writer"] }, ["writer"]), null);
  // The list wins when both keys are present, however many roles were reached.
  assert.match(
    relayRefusal({ required: ["writer"], count: 2 }, ["builder", "auditor"]),
    /missing handoff to: writer/,
  );
});

test("count mode wants distinct downstream roles, not echoes", () => {
  const policy = { required: [], count: 2 };
  const self = "planner";
  const upstream = "supervisor";
  // A send back to the role that assigned this task, and a note to itself, are
  // not handoffs further down the cluster.
  const echoed = relayRefusal(policy, [upstream, self], { role: self, upstream });
  assert.match(echoed, /missing handoff: 2 of 2 required distinct downstream roles/);
  assert.match(echoed, /delivered downstream: none/);

  const one = relayRefusal(policy, [upstream, self, "builder"], { role: self, upstream });
  assert.match(one, /missing handoff: 1 of 2 required distinct downstream roles/);
  assert.match(one, /delivered downstream: builder/);

  const enough = relayRefusal(policy, [upstream, self, "builder", "writer"], { role: self, upstream });
  assert.equal(enough, null);
  // Two handoffs to the same role are one edge, not two.
  const repeated = relayRefusal(policy, ["builder", "builder"], { role: self, upstream });
  assert.match(repeated, /missing handoff: 1 of 2/);
});

test("no policy, or a policy that guards nothing, never refuses", () => {
  assert.equal(relayRefusal(DEFAULT_RELAY, []), null);
  assert.equal(relayRefusal({ required: [], count: 0 }, []), null);
  assert.equal(relayRefusal(undefined, []), null);
  assert.equal(relayEnabled({ required: [], count: null }), false);
});
