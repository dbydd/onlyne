import assert from "node:assert/strict";
import { test } from "node:test";
import { createCommands, matchRowsByPrefix } from "./commands.mjs";
import { collectNotifications, fakeOrca } from "./testing.mjs";

function boardWith(rows, overrides = {}) {
  return {
    scannedAt: 1_789_000_000_000,
    ok: true,
    errors: [],
    tabs: [],
    roots: [],
    strayTabs: [],
    rows,
    summary: {
      roots: 0,
      rootsFailed: 0,
      roles: 1,
      tabs: rows.filter((item) => item.live).length,
      liveTabs: rows.filter((item) => item.live).length,
      sessions: rows.length,
      sessionsWorking: 0,
      joined: rows.filter((item) => item.joined).length,
      strayTabs: 0,
      rows: rows.length,
      ...overrides,
    },
  };
}

function row(overrides = {}) {
  return {
    kind: "task",
    root: "/srv/onlyne-a",
    role: "planner",
    taskId: "task-alpha",
    sessionId: "sess-alpha",
    paneKey: "45e603f7-0772-48aa-bcf6-832272747713:b6d067b6-9255-4f5c-a13f-24f194ea0560",
    handle: "term_11111111-1111-4111-8111-111111111111",
    selector: "2ea2fe23-829c-4a8f-bcac-4129eb78a164",
    joined: true,
    live: true,
    session: { lifecycle: "working", agent: "running", updatedAt: "2026-09-11T00:01:00Z" },
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
  assert.match(sent.at(-1).body, /已切到 task-alpha（planner）/);
});

test("a pane prefix selects a row too, in full or shortened form", async () => {
  const { commands, orca } = setup({
    rows: [row({ taskId: "task-alpha" }), row({ taskId: "task-beta", paneKey: "tab-2:leaf-2", handle: "term_2" })],
  });
  const full = await commands.focus({ task: "45e603f7-0772-48aa-bcf6-832272747713:b6d067b6" });
  assert.equal(full.ok, true);
  assert.deepEqual(orca.calls.switchTerminal, ["term_11111111-1111-4111-8111-111111111111"]);

  const short = await commands.focus({ task: "45e603f7:b6d067b6" });
  assert.equal(short.ok, true);
  assert.equal(short.paneKey, row().paneKey);
  assert.equal(orca.calls.switchTerminal.length, 2);
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
  assert.match(sent.at(-1).body, /2 行命中 前缀 "task-al"/);
  assert.match(sent.at(-1).body, /task-alpha \(planner\) @\/srv\/onlyne-a/);
});

test("an unmatched prefix reports rather than switching", async () => {
  const { commands, orca, sent } = setup();
  const result = await commands.focus({ task: "task-nope" });
  assert.equal(result.ok, false);
  assert.equal(result.code, "no_match");
  assert.deepEqual(orca.calls.switchTerminal, []);
  assert.match(sent.at(-1).body, /没有行的 task 或 pane_key 以 "task-nope" 开头/);
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

test("a row with no tab on the tab axis reports instead of switching", async () => {
  const { commands, orca, sent } = setup({
    rows: [row({ joined: false, live: false, handle: null, paneKey: null, selector: null })],
  });
  const result = await commands.focus({ task: "task-alpha" });
  assert.equal(result.ok, false);
  assert.equal(result.code, "no_handle");
  assert.deepEqual(orca.calls.switchTerminal, []);
  assert.match(sent.at(-1).body, /没有 handle/);
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

test("copy-agent-context emits pane_key/handle/selector with selector from worktreeId", async () => {
  const { commands, sent, logs } = setup();
  const result = await commands.copyAgentContext({ task: "task-alpha" });
  assert.equal(result.ok, true);
  assert.equal(result.taskId, "task-alpha");
  assert.equal(result.paneKey, row().paneKey);
  assert.equal(result.handle, "term_11111111-1111-4111-8111-111111111111");
  assert.equal(result.selector, "2ea2fe23-829c-4a8f-bcac-4129eb78a164");
  assert.match(sent.at(-1).body, /pane_key: 45e603f7-0772/);
  assert.match(sent.at(-1).body, /orca selector: 2ea2fe23-829c-4a8f-bcac-4129eb78a164/);
  assert.ok(logs.some((line) => line.includes("handle: term_")));
});

test("copy-agent-context omits the selector a tab row never carried", async () => {
  const { commands, sent } = setup({
    rows: [
      row({
        kind: "tab",
        taskId: null,
        role: null,
        paneKey: "470e41ba-86b3-43b4-86c3-46c634619a07:31f6d4b1-7192-4e59-b90b-2e8962d72353",
        handle: "term_2",
        selector: null,
      }),
    ],
  });
  const result = await commands.copyAgentContext({ task: "470e41ba" });
  assert.equal(result.ok, true);
  assert.equal(result.selector, null);
  assert.equal(result.taskId, null);
  assert.equal(
    result.context,
    "pane_key: 470e41ba-86b3-43b4-86c3-46c634619a07:31f6d4b1-7192-4e59-b90b-2e8962d72353\nhandle: term_2"
  );
  assert.equal(/orca selector/.test(sent.at(-1).body), false);
});

test("the board command pushes the rendered board to the notification channel", async () => {
  const { commands, sent, logs } = setup();
  const result = await commands.board({});
  assert.equal(result.ok, true);
  assert.equal(sent.at(-1).title, "Onlyne sessions");
  assert.match(sent.at(-1).body, /0 roots · 1 roles · 1 tabs \(1 live\) · 1 sessions \(0 working\)/);
  assert.ok(logs.length > 0);
});

test("refresh rescans and pushes the fresh board", async () => {
  const { commands, sent } = setup();
  const result = await commands.refresh({});
  assert.equal(result.ok, true);
  assert.equal(sent.length, 1);
});

test("an empty board names the missing configuration", async () => {
  const { commands, sent } = setup({ board: boardWith([]) });
  await commands.board({});
  assert.match(sent.at(-1).body, /还没有 serverRoots/);
});

test("matchRowsByPrefix ignores removed rows and matches case-insensitively", () => {
  const rows = [
    row({ taskId: "task-alpha" }),
    row({ taskId: "task-dead", removed: true, live: false }),
  ];
  const match = matchRowsByPrefix(rows, "TASK-AL");
  assert.equal(match.status, "unique");
  assert.equal(match.matches[0].taskId, "task-alpha");
  assert.equal(matchRowsByPrefix(rows, "task-nope").status, "none");
  assert.equal(matchRowsByPrefix([], "").status, "none");
});
