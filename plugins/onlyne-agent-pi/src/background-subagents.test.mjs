import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";

import { createSubagentProbe } from "./background-subagents.mjs";

const SESSION = "s-ours";
const OTHER = "s-theirs";

/** A throwaway home with a missions registry, as the extension leaves it. */
function homeWith(files) {
  const homeDir = fs.mkdtempSync(path.join(os.tmpdir(), "onlyne-subagents-"));
  const missions = path.join(homeDir, ".pi", "subagents", "missions");
  fs.mkdirSync(missions, { recursive: true });
  for (const [name, body] of Object.entries(files)) {
    fs.writeFileSync(path.join(missions, name), typeof body === "string" ? body : JSON.stringify(body));
  }
  return { homeDir, missions };
}

function probeFor(homeDir, sessionId = SESSION) {
  const logs = [];
  const probe = createSubagentProbe({
    homeDir,
    getSessionId: () => sessionId,
    log: (line) => logs.push(line),
  });
  return { logs, probe };
}

test("a registry of terminal missions reads as not running", async () => {
  const { homeDir } = homeWith({
    "a.json": { id: "a", status: "completed", ownerSessionId: SESSION },
    "b.json": { id: "b", status: "failed", ownerSessionId: SESSION },
    "c.json": { id: "c", status: "cancelled", ownerSessionId: SESSION },
  });
  const { probe } = probeFor(homeDir);

  assert.equal(await probe.running(), false);
});

test("a mission whose status is not terminal reads as running", async () => {
  // The live spelling was never observed, so any unrecognised value is treated as
  // live; that is the safe direction, and this is what it looks like in a test.
  const { homeDir } = homeWith({
    "a.json": { id: "a", status: "running", ownerSessionId: SESSION },
    "b.json": { id: "b", status: "something-new", ownerSessionId: SESSION },
  });
  const { probe } = probeFor(homeDir);

  assert.equal(await probe.running(), true);
});

test("a mission owned by this session reads as running and another session's does not", async () => {
  const theirs = homeWith({ "a.json": { id: "a", status: "running", ownerSessionId: OTHER } });
  const { probe: other } = probeFor(theirs.homeDir, SESSION);
  assert.equal(await other.running(), false, "another session's live mission is not this session's work");

  const ours = homeWith({ "a.json": { id: "a", status: "running", ownerSessionId: SESSION } });
  const { probe: mine } = probeFor(ours.homeDir, SESSION);
  assert.equal(await mine.running(), true);
});

test("a caller that cannot name its own session counts a mission naming none", async () => {
  const { homeDir } = homeWith({ "a.json": { id: "a", status: "running" } });
  const { probe } = probeFor(homeDir, null);

  assert.equal(await probe.running(), true, "watching a silent subset is worse than watching too much");
});

test("a missing registry reads as not running and does not throw", async () => {
  const homeDir = fs.mkdtempSync(path.join(os.tmpdir(), "onlyne-subagents-empty-"));
  const { logs, probe } = probeFor(homeDir);

  assert.equal(await probe.running(), false);
  assert.equal(logs.length, 1, "the missing registry is said once");
  assert.equal(await probe.running(), false);
  assert.equal(logs.length, 1, "and not repeated on the next call");
});

test("a missions path that is a file rather than a directory reads as not running", async () => {
  const homeDir = fs.mkdtempSync(path.join(os.tmpdir(), "onlyne-subagents-file-"));
  const missions = path.join(homeDir, ".pi", "subagents");
  fs.mkdirSync(missions, { recursive: true });
  fs.writeFileSync(path.join(missions, "missions"), "not a directory");
  const { probe } = probeFor(homeDir);

  assert.equal(await probe.running(), false);
});

test("an unreadable mission file reads as not running and does not throw", async () => {
  const { homeDir, missions } = homeWith({ "a.json": { id: "a", status: "completed" } });
  fs.writeFileSync(path.join(missions, "b.json"), "{ not json");
  fs.mkdirSync(path.join(missions, "c.json")); // a directory where a file is expected
  fs.chmodSync(missions, 0o000);
  const { probe } = probeFor(homeDir);
  try {
    assert.equal(await probe.running(), false);
  } finally {
    fs.chmodSync(missions, 0o755);
  }
});

test("a mission with no status reads as not running", async () => {
  const { homeDir } = homeWith({
    "a.json": { id: "a", ownerSessionId: SESSION },
    "b.json": { id: "b", status: null, ownerSessionId: SESSION },
    "c.json": { id: "c", status: 42, ownerSessionId: SESSION },
  });
  const { probe } = probeFor(homeDir);

  assert.equal(await probe.running(), false);
});

test("malformed JSON in a live-looking registry still answers", async () => {
  const { homeDir, missions } = homeWith({ "a.json": "<<<not json>>>" });
  fs.writeFileSync(path.join(missions, "b.json"), JSON.stringify({ status: "running", ownerSessionId: SESSION }));
  const { probe } = probeFor(homeDir);

  assert.equal(await probe.running(), true, "one bad file does not hide a live mission");
});

test("a mission larger than the byte bound is skipped, and the rest of the registry still answers", async () => {
  const { homeDir, missions } = homeWith({
    "a.json": { id: "a", status: "completed", ownerSessionId: SESSION, summary: "x".repeat(4096) },
  });
  fs.writeFileSync(path.join(missions, "huge.json"), JSON.stringify({
    status: "running",
    ownerSessionId: SESSION,
    transcript: "x".repeat(300 * 1024),
  }));
  const { logs, probe } = probeFor(homeDir);

  assert.equal(await probe.running(), false);
  assert.equal(logs.some((line) => line.includes("too large")), true);
});

test("a capped read spends its budget on the newest missions", async () => {
  // The cap is asserted by which file the probe read, so each case puts the live
  // mission on one side of the cap and the terminal one on the other.
  const registry = {
    "old.json": { id: "old", status: "running", ownerSessionId: SESSION },
    "new.json": { id: "new", status: "completed", ownerSessionId: SESSION },
  };
  const { homeDir, missions } = homeWith(registry);
  const old = new Date(Date.now() - 60_000);
  fs.utimesSync(path.join(missions, "old.json"), old, old);
  const capped = createSubagentProbe({ homeDir, getSessionId: () => SESSION, maxFiles: 1 });

  // Only the newer file is inside the cap, and it is terminal.
  assert.equal(await capped.running(), false);

  // The same cap, the same two files, with the newer one now live: the answer
  // flips only because the newer file is the one that got read.
  fs.writeFileSync(
    path.join(missions, "new.json"),
    JSON.stringify({ id: "new", status: "running", ownerSessionId: SESSION }),
  );
  assert.equal(await createSubagentProbe({
    homeDir,
    getSessionId: () => SESSION,
    maxFiles: 1,
  }).running(), true);
});
