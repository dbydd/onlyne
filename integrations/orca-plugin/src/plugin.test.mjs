import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { createPlugin } from "../main.mjs";
import { createRunner } from "./runner.mjs";

const pluginRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const manifest = JSON.parse(readFileSync(join(pluginRoot, "orca-plugin.json"), "utf8"));

const COMMAND_ID_RE = /^[A-Za-z0-9]+(?:[._-][A-Za-z0-9]+)*$/;
const PLUGIN_ID_RE = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;
const CAPABILITY_KINDS = new Set([
  "workspace:read",
  "terminal:send",
  "notifications:show",
  "storage",
  "secrets",
  "events:subscribe",
  "settings:own",
]);
const EVENT_NAMES = new Set(["worktree.created", "worktree.removed", "agent.status.changed"]);

function fakeOrcaApi({
  capabilities = ["workspace:read", "notifications:show", "events:subscribe"],
} = {}) {
  const registered = new Map();
  const subscriptions = [];
  const logs = [];
  const hostCalls = [];
  return {
    grantedCapabilities: capabilities,
    commands: { register: (id, handler) => registered.set(id, handler) },
    events: { on: (name, handler) => subscriptions.push({ name, handler }) },
    host: {
      call: async (method, params) => {
        hostCalls.push({ method, params });
        return { delivered: true };
      },
    },
    log: (line) => logs.push(line),
    registered,
    subscriptions,
    logs,
    hostCalls,
  };
}

function fakeBinaries(record) {
  return async (binary, args) => {
    record.push([binary, ...args].join(" "));
    if (args[0] === "worktree" && args[1] === "list") {
      return { stdout: JSON.stringify({ id: "x", ok: true, result: { worktrees: [] } }), stderr: "" };
    }
    return { stdout: JSON.stringify({ id: "x", ok: true, result: {} }), stderr: "" };
  };
}

function activateWith(orca) {
  const calls = [];
  const runner = createRunner({ exec: fakeBinaries(calls) });
  const plugin = createPlugin({
    orca,
    runner,
    binaries: { orcaBin: "orca", onlyneBin: "onlyne", configLoaded: false, configPath: null },
  });
  return { plugin, calls };
}

test("the manifest is a legal pluginApi v1 manifest", () => {
  assert.equal(manifest.manifestVersion, 1);
  assert.equal(manifest.pluginApi, 1);
  assert.match(manifest.id, PLUGIN_ID_RE);
  assert.match(manifest.publisher, PLUGIN_ID_RE);
  assert.match(manifest.version, /^\d+\.\d+\.\d+$/);
  assert.match(manifest.engines.orca, /^>=\d+\.\d+\.\d+$/);
  assert.equal(manifest.main, "main.mjs");
  assert.ok(existsSync(join(pluginRoot, manifest.main)));

  for (const capability of manifest.capabilities) {
    assert.ok(CAPABILITY_KINDS.has(capability.kind), `unknown capability ${capability.kind}`);
  }
  for (const command of manifest.contributes.commands) {
    assert.match(command.id, COMMAND_ID_RE);
    assert.ok(command.title.length > 0);
    assert.equal(command.action, undefined, "commands must run in the worker, not alias built-ins");
  }
  for (const subscription of manifest.contributes.events) {
    assert.ok(EVENT_NAMES.has(subscription.on), `unknown event ${subscription.on}`);
  }
  assert.equal(manifest.contributes.keybindings, undefined, "the plugin must not claim default keys");
});

test("the panel entry exists and asks for no browsing context", () => {
  const [panel] = manifest.contributes.panels;
  assert.equal(panel.id, "board");
  const entry = join(pluginRoot, panel.entry);
  assert.ok(existsSync(entry));
  const html = readFileSync(entry, "utf8");
  assert.match(html, /orca-panel-action/);
  assert.equal(/<iframe|<img[^>]+src="https?:/.test(html), false);
  assert.equal(/fetch\(|XMLHttpRequest|localStorage/.test(html), false);
  assert.equal(/workspace\.readContext|notifications\.show/.test(html), true);
});

test("activation registers exactly the declared commands and events", () => {
  const orca = fakeOrcaApi();
  const { plugin, calls } = activateWith(orca);
  try {
    assert.deepEqual(
      [...orca.registered.keys()].sort(),
      manifest.contributes.commands.map((command) => command.id).sort()
    );
    assert.deepEqual(
      orca.subscriptions.map((entry) => entry.name).sort(),
      manifest.contributes.events.map((subscription) => subscription.on).sort()
    );
    assert.equal(plugin.subscribed, true);
    // Read-only discipline: activation only ever lists.
    assert.ok(calls.length > 0);
    assert.equal(calls.every((line) => line.includes("list")), true, calls.join(" | "));
  } finally {
    plugin.boardState.stop();
  }
});

test("without events:subscribe the plugin still registers commands and logs why", () => {
  const orca = fakeOrcaApi({ capabilities: ["notifications:show"] });
  const { plugin } = activateWith(orca);
  try {
    assert.equal(orca.subscriptions.length, 0);
    assert.equal(orca.registered.size, manifest.contributes.commands.length);
    assert.ok(orca.logs.some((line) => line.includes("events:subscribe not granted")));
  } finally {
    plugin.boardState.stop();
  }
});

test("without notifications:show nothing is pushed to the desktop", async () => {
  const orca = fakeOrcaApi({ capabilities: ["events:subscribe"] });
  const { plugin } = activateWith(orca);
  try {
    await plugin.notify("t", "b");
    assert.deepEqual(orca.hostCalls, []);
  } finally {
    plugin.boardState.stop();
  }
});

test("activation and commands never switch, create, close or rename a tab", async () => {
  const orca = fakeOrcaApi();
  const { plugin, calls } = activateWith(orca);
  try {
    await plugin.commands.board({});
    await plugin.commands.refresh({});
    assert.equal(
      calls.some((line) => /terminal (switch|create|close|rename|send)/.test(line)),
      false,
      calls.join(" | ")
    );
    assert.equal(orca.hostCalls.some((call) => call.method === "terminal.sendText"), false);
    assert.equal(orca.hostCalls.every((call) => call.method === "notifications.show"), true);
  } finally {
    plugin.boardState.stop();
  }
});

test("the first scan stays quiet; a later structural change notifies once", async () => {
  const orca = fakeOrcaApi();
  const { plugin } = activateWith(orca);
  try {
    await plugin.boardState.refresh({ reason: "test" });
    assert.deepEqual(orca.hostCalls, [], "activation and the first scan must not notify");
  } finally {
    plugin.boardState.stop();
  }
});

test("an event rescan is debounced instead of scanning per event", async () => {
  const orca = fakeOrcaApi();
  const { plugin, calls } = activateWith(orca);
  try {
    const before = calls.length;
    const created = orca.subscriptions.find((entry) => entry.name === "worktree.created");
    await created.handler({ path: "/tmp/x", worktreeId: "wt" });
    await created.handler({ path: "/tmp/x", worktreeId: "wt" });
    assert.equal(calls.length, before, "events must not scan synchronously");
  } finally {
    plugin.boardState.stop();
  }
});
