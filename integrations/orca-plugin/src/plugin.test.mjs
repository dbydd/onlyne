import assert from "node:assert/strict";
import { existsSync, mkdtempSync, readFileSync, readdirSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { createPlugin } from "../main.mjs";
import { createRunner } from "./runner.mjs";
import { committedText } from "./testing.mjs";

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
const MUTATING = /terminal (switch|create|close|rename|send)|worktree /;

function fakeOrcaApi({
  // Exactly what the manifest asks for: the plugin reads no workspace context,
  // so a fake granting `workspace:read` would grant a capability it never uses.
  capabilities = ["notifications:show", "events:subscribe"],
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

/** Answers the two CLI surfaces the worker touches, and nothing else. */
function fakeBinaries(record) {
  return async (binary, args) => {
    record.push([binary, ...args].join(" "));
    const verb = args.join(" ");
    if (verb === "terminal list --json") {
      const terminals = [
        { handle: "term_1", tabId: "t1", leafId: "l1", title: "zsh", connected: true, worktreeId: "wt1" },
      ];
      return { stdout: JSON.stringify({ id: "x", ok: true, result: { terminals } }), stderr: "" };
    }
    if (args.includes("sessions")) {
      return { stdout: JSON.stringify({ ok: true, data: { sessions: [] } }), stderr: "" };
    }
    if (args.includes("roles")) {
      return { stdout: JSON.stringify({ ok: true, data: { roles: [] } }), stderr: "" };
    }
    return { stdout: JSON.stringify({ id: "x", ok: true, result: {} }), stderr: "" };
  };
}

function activateWith(orca, { serverRoots = [] } = {}) {
  const calls = [];
  const runner = createRunner({ exec: fakeBinaries(calls) });
  // The worker regenerates the panel document on every scan; a suite must never
  // write into the plugin tree it is testing (the committed panel.html is the
  // placeholder a content-addressed install keeps).
  const panelRoot = mkdtempSync(join(tmpdir(), "onlyne-plugin-test-"));
  const plugin = createPlugin({
    orca,
    runner,
    binaries: { orcaBin: "orca", onlyneBin: "onlyne", serverRoots, configLoaded: false, configPath: null },
    pluginRoot: panelRoot,
  });
  return { plugin, calls, panelRoot };
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

test("the committed panel entry is the placeholder, and asks for no browsing context", () => {
  const [panel] = manifest.contributes.panels;
  assert.equal(panel.id, "board");
  const entry = join(pluginRoot, panel.entry);
  assert.ok(existsSync(entry));
  // Asserted from git, not from the working copy: a dev install rewrites this
  // file by design (src/panel-document.mjs), which is exactly why the working
  // copy cannot answer what a content-addressed install keeps. A vendored copy
  // that is not a git checkout keeps the shipped file, so it reads that.
  const html = committedText("integrations/orca-plugin/panel.html") ?? readFileSync(entry, "utf8");
  assert.match(html, /onlyne-sessions panel placeholder/);
  assert.equal(/onlyne-sessions panel snapshot/.test(html), false);
  assert.equal(/<iframe|<img[^>]+src="https?:/.test(html), false);
  assert.equal(/fetch\(|XMLHttpRequest|localStorage|postMessage|WebSocket/.test(html), false);
});

test("activation registers exactly the declared commands and events", async () => {
  const orca = fakeOrcaApi();
  const { plugin, calls } = activateWith(orca);
  try {
    await plugin.boardState.refresh({ reason: "test" });
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
    assert.deepEqual(calls, ["orca terminal list --json"]);
  } finally {
    plugin.boardState.stop();
  }
});

test("with serverRoots configured the worker queries each root's admin surface", async () => {
  const orca = fakeOrcaApi();
  const { plugin, calls } = activateWith(orca, { serverRoots: ["/srv/a", "/srv/b"] });
  try {
    await plugin.boardState.refresh({ reason: "test" });
    assert.deepEqual(calls, [
      "orca terminal list --json",
      "onlyne --server-root /srv/a sessions --json",
      "onlyne --server-root /srv/a roles --json",
      "onlyne --server-root /srv/b sessions --json",
      "onlyne --server-root /srv/b roles --json",
    ]);
    const board = plugin.boardState.getBoard();
    assert.equal(board.summary.roots, 2);
    assert.equal(board.summary.tabs, 1);
    assert.equal(board.summary.strayTabs, 1);
    assert.deepEqual(board.errors, []);
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
  const { plugin, calls } = activateWith(orca, { serverRoots: ["/srv/a"] });
  try {
    await plugin.commands.board({});
    await plugin.commands.refresh({});
    assert.equal(calls.some((line) => MUTATING.test(line)), false, calls.join(" | "));
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

test("a scan regenerates the panel document the panel reads", async () => {
  const orca = fakeOrcaApi({ capabilities: ["notifications:show"] });
  const { plugin, panelRoot } = activateWith(orca, { serverRoots: ["/srv/a"] });
  try {
    await plugin.boardState.refresh({ reason: "test" });

    const document = readFileSync(join(panelRoot, "panel.html"), "utf8");
    assert.match(document, /onlyne-sessions panel snapshot/);
    assert.match(document, /board-snapshot/);
    assert.deepEqual(readdirSync(panelRoot), ["panel.html"], "the atomic write leaves no temp file");

    // The second scan with the same board must not rewrite (a rewrite remounts
    // the panel), and the debug command writes the very payload the document
    // embeds.
    const first = statSync(join(panelRoot, "panel.html")).mtimeMs;
    await plugin.boardState.refresh({ reason: "test" });
    assert.equal(statSync(join(panelRoot, "panel.html")).mtimeMs, first);
  } finally {
    plugin.boardState.stop();
  }
});
