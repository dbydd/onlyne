// `.pi/onlyne.json` and the session-identity gate.
//
// The identity gate is the hard requirement that lets one pi installation serve
// both onlyne sessions and ordinary ones: with any of the three variables
// missing the extension must stay inert.

import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { afterEach, test } from "node:test";

import { CONFIG_RELATIVE_PATH, DEFAULT_CONFIG, loadConfig, sessionIdentity } from "./config.mjs";

const cleanups = [];
afterEach(() => {
  while (cleanups.length > 0) cleanups.pop()();
});

/** One temp workspace, optionally carrying a `.pi/onlyne.json` body. */
function workspace(body) {
  const dir = mkdtempSync(join(tmpdir(), "pi-onlyne-config-"));
  cleanups.push(() => rmSync(dir, { recursive: true, force: true }));
  if (body !== undefined) {
    mkdirSync(join(dir, ".pi"), { recursive: true });
    writeFileSync(join(dir, CONFIG_RELATIVE_PATH), body);
  }
  return dir;
}

test("a missing switch file means enabled and autoStart", () => {
  const dir = workspace();
  const config = loadConfig(dir);
  assert.deepEqual(
    { enabled: config.enabled, autoStart: config.autoStart },
    { enabled: DEFAULT_CONFIG.enabled, autoStart: DEFAULT_CONFIG.autoStart },
  );
  assert.equal(config.present, false);
  assert.equal(config.warning, null);
  assert.equal(config.path, join(dir, ".pi", "onlyne.json"));
});

test("the switch file's two keys are honoured", () => {
  const dir = workspace(JSON.stringify({ enabled: false, watch: { autoStart: false } }));
  const config = loadConfig(dir);
  assert.equal(config.enabled, false);
  assert.equal(config.autoStart, false);
  assert.equal(config.present, true);

  const partial = workspace(JSON.stringify({ watch: { autoStart: false } }));
  assert.deepEqual(
    { enabled: loadConfig(partial).enabled, autoStart: loadConfig(partial).autoStart },
    { enabled: true, autoStart: false },
  );
});

test("a malformed switch file warns and falls back instead of disabling the session", () => {
  const broken = workspace("{not json");
  const brokenConfig = loadConfig(broken);
  assert.equal(brokenConfig.enabled, true);
  assert.equal(brokenConfig.autoStart, true);
  assert.match(brokenConfig.warning, /not valid JSON/);

  const scalar = workspace('"on"');
  const scalarConfig = loadConfig(scalar);
  assert.equal(scalarConfig.enabled, true);
  assert.match(scalarConfig.warning, /must hold a JSON object/);
});

test("the session identity gate needs all three variables", () => {
  const full = { ONLYNE_ROLE: "planner", ONLYNE_SESSION_ID: "s1", ONLYNE_TASK_ID: "t1" };
  assert.deepEqual(sessionIdentity(full), { role: "planner", sessionId: "s1", taskId: "t1" });
  for (const missing of ["ONLYNE_ROLE", "ONLYNE_SESSION_ID", "ONLYNE_TASK_ID"]) {
    const partial = { ...full };
    delete partial[missing];
    assert.equal(sessionIdentity(partial), null, `${missing} alone must disable the plugin`);
  }
  assert.equal(sessionIdentity({}), null);
  assert.equal(sessionIdentity({ ONLYNE_ROLE: "", ONLYNE_SESSION_ID: "s", ONLYNE_TASK_ID: "t" }), null);
});

test("the shipped switch file documents only the keys the plugin reads", () => {
  const path = fileURLToPath(new URL("../onlyne.json.example", import.meta.url));
  const example = JSON.parse(readFileSync(path, "utf8"));
  assert.deepEqual(example, { enabled: true, watch: { autoStart: true } });
});
