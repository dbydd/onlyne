import assert from "node:assert/strict";
import { after, test } from "node:test";
import { createCommands, matchRowsByTaskPrefix } from "./commands.mjs";
import { collectNotifications, fakeOrca } from "./testing.mjs";

after(() => {});

function boardWith(rows, summary = {}) {
  return {
    scannedAt: 1_789_000_000_000,
    ok: true,
    errors: [],
    workspaces: [],
    rows,
    groups: [],
    summary: { roles: 1, liveTabs: rows.filter((row) => row.live).length, sessionsWorking: 0, removed: 0, rows: rows.length, ...summary },
  };
}

function row(overrides = {}) {
  return {
    role: "planner",
    taskId: "task-alpha",
    sessionId: "sess-alpha",
    paneKey: "tab-1:leaf-1",
    handle: "term_11111111-1111-4111-8111-111111111111",
    selector: "path:/tmp/role-planner",
    mappingState: "spawned",
    removed: false,
    live: true,
    terminal: { handle: "term_11111111-1111-4111-8111-111111111111", connected: true, lastOutputAt: 1_789_000_000_000 },
    session: { lifecycle: "working", agent: "running" },
    ...overrides,
  };
}

function setup({ rows = [row()], switchFailure = null, board = null } = {}) {
  const { sent, notify } = collectNotifications();
  const orca = fakeOrca({ switchFailure });
  const state = { board: board ?? boardWith(rows) };
  const logs = [];
  const commands = createCommands({
    getBoard: () => state.board,
    refreshBoard: async () => state.board,
    orca,
    notify,
    log: (line) => logs.push(line),
    now: () => 1_789_000_000_000,
  });
  return { commands, orca, sent, notify, logs, state };
}

test("focus switches only when the task prefix matches exactly one row", async () => {
  const { commands, orca, sent } = setup({
    rows: [row({ taskId: "task-alpha" }), row({ taskId: "task-beta", paneKey: "tab-2:leaf-2", handle: "term_2" })],
  });
  const result = await commands.focus({ task: "task-al" });
  assert.equal(result.ok, true);
  assert.deepEqual(orca.calls.switchTerminal, ["term_11111111-1111-4111-8111-111111111111"]);
  assert.match(sent.at(-1).body, /已切到 task-alpha/);
});

test("an ambiguous task prefix switches nothing and lists the candidates", async () => {
  const { commands, orca, sent } = setup({
    rows: [row({ taskId: "task-alpha" }), row({ taskId: "task-alpine", paneKey: "tab-2:leaf-2", handle: "term_2" })],
  });
  const result = await commands.focus({ task: "task-al" });
  assert.equal(result.ok, false);
  assert.equal(result.code, "ambiguous");
  assert.equal(result.matches, 2);
  assert.deepEqual(orca.calls.switchTerminal, []);
  assert.match(sent.at(-1).body, /2 个 tab 命中/);
  assert.match(sent.at(-1).body, /task-alpha/);
});

test("an unmatched prefix reports rather than switching", async () => {
  const { commands, orca, sent } = setup();
  const result = await commands.focus({ task: "task-nope" });
  assert.equal(result.ok, false);
  assert.equal(result.code, "no_match");
  assert.deepEqual(orca.calls.switchTerminal, []);
  assert.match(sent.at(-1).body, /没有 task 以 "task-nope" 开头/);
});

test("focus without a prefix uses the unique live tab and refuses when ambiguous", async () => {
  const single = setup();
  assert.equal((await single.commands.focus({})).ok, true);
  assert.equal(single.orca.calls.switchTerminal.length, 1);

  const many = setup({
    rows: [row({ taskId: "task-alpha" }), row({ taskId: "task-beta", paneKey: "tab-2:leaf-2", handle: "term_2" })],
  });
  const refused = await many.commands.focus({});
  assert.equal(refused.ok, false);
  assert.equal(refused.code, "ambiguous");
  assert.deepEqual(many.orca.calls.switchTerminal, []);
});

test("a stale handle reports the CLI error instead of pretending success", async () => {
  const { commands, sent } = setup({
    switchFailure: { ok: false, code: "terminal_handle_stale", message: "no such terminal" },
  });
  const result = await commands.focus({ task: "task-alpha" });
  assert.equal(result.ok, false);
  assert.equal(result.code, "terminal_handle_stale");
  assert.match(sent.at(-1).body, /terminal_handle_stale/);
});

test("copy-agent-context emits the pane_key/handle/selector triple as a notification", async () => {
  const { commands, sent, logs } = setup();
  const result = await commands.copyAgentContext({ task: "task-alpha" });
  assert.equal(result.ok, true);
  assert.equal(result.paneKey, "tab-1:leaf-1");
  assert.equal(result.handle, "term_11111111-1111-4111-8111-111111111111");
  assert.equal(result.selector, "path:/tmp/role-planner");
  assert.match(sent.at(-1).body, /pane_key: tab-1:leaf-1/);
  assert.match(sent.at(-1).body, /orca selector: path:\/tmp\/role-planner/);
  assert.ok(logs.some((line) => line.includes("handle: term_")));
});

test("the board command pushes the rendered board to the notification channel", async () => {
  const { commands, sent, logs } = setup();
  const result = await commands.board({});
  assert.equal(result.ok, true);
  assert.equal(sent.at(-1).title, "Onlyne sessions");
  assert.match(sent.at(-1).body, /Onlyne sessions · 1 roles · 1 live tabs · 0 working/);
  assert.ok(logs.length > 0);
});

test("refresh rescans and pushes the fresh board", async () => {
  const { commands, sent } = setup();
  const result = await commands.refresh({});
  assert.equal(result.ok, true);
  assert.equal(sent.length, 1);
});

test("an empty board notifies the wait-for-supervisor sentence", async () => {
  const { commands, sent } = setup({ board: boardWith([]) });
  await commands.board({});
  assert.match(sent.at(-1).body, /等 supervisor 拉起 role client/);
});

test("matchRowsByTaskPrefix ignores removed rows and matches case-insensitively", () => {
  const rows = [
    row({ taskId: "task-alpha" }),
    row({ taskId: "task-dead", removed: true, live: false }),
  ];
  const match = matchRowsByTaskPrefix(rows, "TASK-AL");
  assert.equal(match.status, "unique");
  assert.equal(match.matches[0].taskId, "task-alpha");
  assert.equal(matchRowsByTaskPrefix(rows, "task-nope").status, "none");
});
