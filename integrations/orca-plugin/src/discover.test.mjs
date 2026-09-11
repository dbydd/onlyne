import assert from "node:assert/strict";
import { after, test } from "node:test";
import { collectBoard, dedupeWorktrees, indexSessions, markRemoved, summarize } from "./discover.mjs";
import { clientSocketPath } from "./onlyne-cli.mjs";
import {
  cleanupAll,
  fakeOnlyne,
  fakeOrca,
  makeDir,
  makeWorkspace,
  mappingRow,
  sessionRow,
  terminalRow,
} from "./testing.mjs";

after(cleanupAll);

function worktree(path, overrides = {}) {
  return { path, worktreeId: `wt::${path}`, displayName: path.split("/").pop(), isBare: false, ...overrides };
}

/** Two role workspaces plus one plain worktree without a mapping file. */
function twoRoleFixture() {
  const planner = makeWorkspace({
    name: "planner",
    socket: true,
    lines: [
      mappingRow({ role: "planner", task_id: "task-alpha", pane_key: "tab-1:leaf-1" }),
      mappingRow({
        role: "planner",
        task_id: "task-beta",
        pane_key: "tab-2:leaf-2",
        session_id: "sess-beta",
        handle: "term_22222222-2222-4222-8222-222222222222",
      }),
      mappingRow({ role: "planner", task_id: "task-dead", pane_key: "tab-3:leaf-3", state: "closed" }),
    ],
  });
  const builder = makeWorkspace({
    name: "builder",
    socket: true,
    lines: [
      mappingRow({
        role: "builder",
        task_id: "task-gamma",
        pane_key: "tab-4:leaf-4",
        handle: "term_44444444-4444-4444-8444-444444444444",
        session_id: "sess-gamma",
      }),
    ],
  });
  const plain = makeDir({ name: "plain" });
  return { planner, builder, plain };
}

test("only worktrees carrying the mapping file are role workspaces", async () => {
  const { planner, builder, plain } = twoRoleFixture();
  const board = await collectBoard({
    orca: fakeOrca({ worktrees: [worktree(planner), worktree(plain), worktree(builder)], terminalsByPath: {} }),
    onlyne: fakeOnlyne(),
  });
  assert.deepEqual(
    board.workspaces.map((entry) => entry.path).sort(),
    [planner, builder].sort()
  );
  assert.equal(board.rows.length, 3);
  assert.equal(board.summary.roles, 2);
});

test("a worktree without a mapping file never reaches the terminal scan", async () => {
  const { planner, plain } = twoRoleFixture();
  const orca = fakeOrca({ worktrees: [worktree(planner), worktree(plain)], terminalsByPath: {} });
  await collectBoard({ orca, onlyne: fakeOnlyne() });
  assert.deepEqual(orca.calls.listTerminals, [`path:${planner}`]);
});

test("joins mapping rows against live terminals by pane_key derived from tabId:leafId", async () => {
  const { planner } = twoRoleFixture();
  const board = await collectBoard({
    orca: fakeOrca({
      worktrees: [worktree(planner)],
      terminalsByPath: {
        // The CLI row carries no paneKey field on Orca 1.4.198.
        [planner]: [
          terminalRow({ tabId: "tab-1", leafId: "leaf-1", handle: "term_11111111-1111-4111-8111-111111111111" }),
          terminalRow({
            tabId: "tab-3",
            leafId: "leaf-3",
            handle: "term_33333333-3333-4333-8333-333333333333",
            connected: false,
          }),
        ],
      },
    }),
    onlyne: fakeOnlyne(),
  });
  const alpha = board.rows.find((row) => row.taskId === "task-alpha");
  assert.equal(alpha.live, true);
  assert.equal(alpha.terminal.connected, true);
  assert.equal(alpha.paneKey, "tab-1:leaf-1");
  assert.equal(board.summary.liveTabs, 1);
  // The tombstoned pane is not a row at all, even though its terminal exists.
  assert.equal(board.rows.some((row) => row.taskId === "task-dead"), false);
});

test("a mapping row whose tab is gone stays visible but is not live", async () => {
  const { builder } = twoRoleFixture();
  const board = await collectBoard({
    orca: fakeOrca({ worktrees: [worktree(builder)], terminalsByPath: { [builder]: [] } }),
    onlyne: fakeOnlyne(),
  });
  assert.equal(board.rows.length, 1);
  assert.equal(board.rows[0].live, false);
  assert.equal(board.rows[0].terminal, null);
  assert.equal(board.summary.liveTabs, 0);
});

test("session state comes from the workspace client socket and marks working tabs", async () => {
  const { planner } = twoRoleFixture();
  const onlyne = fakeOnlyne({
    sessionsBySocket: {
      [clientSocketPath(planner)]: [
        sessionRow({ task_id: "task-alpha", public_lifecycle: "working", projection: { lifecycle: "working", agent: "running" } }),
      ],
    },
  });
  const board = await collectBoard({
    orca: fakeOrca({
      worktrees: [worktree(planner)],
      terminalsByPath: { [planner]: [terminalRow(), terminalRow({ tabId: "tab-2", leafId: "leaf-2" })] },
    }),
    onlyne,
  });
  const alpha = board.rows.find((row) => row.taskId === "task-alpha");
  assert.equal(alpha.sessionSource, "onlyne");
  assert.equal(alpha.session.lifecycle, "working");
  assert.equal(alpha.session.agent, "running");
  assert.equal(board.summary.sessionsWorking, 1);
  assert.deepEqual(onlyne.calls, [clientSocketPath(planner)]);
});

test("a dead onlyne client degrades to the mapping state instead of failing the scan", async () => {
  const { planner } = twoRoleFixture();
  const board = await collectBoard({
    orca: fakeOrca({
      worktrees: [worktree(planner)],
      terminalsByPath: { [planner]: [terminalRow()] },
    }),
    onlyne: fakeOnlyne({ failure: { ok: false, code: "cli_surface_mismatch", message: "onlyne: unexpected argument '--socket'" } }),
  });
  assert.equal(board.ok, true);
  const alpha = board.rows.find((row) => row.taskId === "task-alpha");
  assert.equal(alpha.session, null);
  assert.equal(alpha.sessionSource, "mapping");
  assert.equal(alpha.live, true, "liveness does not depend on session state");
  const workspace = board.workspaces[0];
  assert.equal(workspace.sessionScan.source, "mapping");
  assert.match(workspace.sessionScan.message, /unexpected argument/);
});

test("a failing terminal scan degrades the workspace without dropping its rows", async () => {
  const { planner } = twoRoleFixture();
  const board = await collectBoard({
    orca: fakeOrca({
      worktrees: [worktree(planner)],
      terminalFailure: { ok: false, code: "selector_not_found", message: "selector not found" },
    }),
    onlyne: fakeOnlyne(),
  });
  assert.equal(board.rows.length, 2);
  assert.equal(board.summary.liveTabs, 0);
  assert.equal(board.errors[0].code, "selector_not_found");
});

test("a failing worktree list reports an empty board with the error", async () => {
  const board = await collectBoard({
    orca: fakeOrca({
      worktreeListFailure: { ok: false, code: "missing_binary", message: "binary not found on PATH" },
    }),
    onlyne: fakeOnlyne(),
  });
  assert.equal(board.ok, false);
  assert.equal(board.rows.length, 0);
  assert.equal(board.summary.roles, 0);
  assert.equal(board.errors[0].scope, "orca");
});

test("duplicate worktree rows for one path collapse to a single scan", async () => {
  const { planner } = twoRoleFixture();
  const orca = fakeOrca({
    worktrees: [worktree(planner), worktree(planner, { worktreeId: `wt2::${planner}` }), worktree(planner, { isBare: true })],
    terminalsByPath: { [planner]: [terminalRow()] },
  });
  const board = await collectBoard({ orca, onlyne: fakeOnlyne() });
  assert.equal(orca.calls.listTerminals.length, 1);
  assert.equal(board.workspaces.length, 1);
  assert.equal(board.rows.length, 2);
  assert.equal(dedupeWorktrees([{ path: "/a", isBare: true }, { path: "/a", isBare: false }])[0].isBare, false);
});

test("a removed worktree keeps its rows, greyed, until the entry expires", async () => {
  const { planner } = twoRoleFixture();
  const live = await collectBoard({
    orca: fakeOrca({ worktrees: [worktree(planner)], terminalsByPath: { [planner]: [terminalRow()] } }),
    onlyne: fakeOnlyne(),
  });
  const removedAt = live.scannedAt;
  const graveyard = new Map([[planner, markRemoved(live, planner, { now: () => removedAt })]]);

  const removedBoard = await collectBoard({
    orca: fakeOrca({ worktrees: [] }),
    onlyne: fakeOnlyne(),
    graveyard,
    now: () => removedAt + 1000,
  });
  assert.equal(removedBoard.rows.length, 2);
  assert.equal(removedBoard.rows.every((row) => row.removed === true), true);
  assert.equal(removedBoard.rows.every((row) => row.live === false), true);
  assert.equal(removedBoard.groups.every((group) => group.removed === true), true);
  assert.equal(removedBoard.summary.roles, 1);

  const expired = await collectBoard({
    orca: fakeOrca({ worktrees: [] }),
    onlyne: fakeOnlyne(),
    graveyard,
    now: () => removedAt + 11 * 60 * 1000,
  });
  assert.equal(expired.rows.length, 0);
  assert.equal(expired.summary.roles, 0);
});

test("a graveyard entry disappears once the worktree is discovered again", async () => {
  const { planner } = twoRoleFixture();
  const graveyard = new Map([
    [planner, { rows: [{ role: "planner", taskId: "task-alpha", worktreePath: planner, removed: true }], removedAt: 1 }],
  ]);
  const board = await collectBoard({
    orca: fakeOrca({ worktrees: [worktree(planner)], terminalsByPath: { [planner]: [terminalRow()] } }),
    onlyne: fakeOnlyne(),
    graveyard,
  });
  assert.equal(graveyard.size, 0);
  assert.equal(board.rows.filter((row) => row.removed).length, 0);
});

test("session matching uses task id then session id, and never guesses by role", () => {
  const index = indexSessions([
    { taskId: "task-a", sessionId: "sess-a", role: "planner" },
    { taskId: "task-b", sessionId: "sess-b", role: "planner" },
  ]);
  assert.equal(index.lookup({ taskId: "task-b", role: "planner" }).sessionId, "sess-b");
  assert.equal(index.lookup({ taskId: "task-zz", sessionId: "sess-a", role: "nobody" }).sessionId, "sess-a");
  assert.equal(index.lookup({ taskId: "task-a", sessionId: "sess-b", role: "planner" }).sessionId, "sess-a");
  assert.equal(index.lookup({ taskId: "task-zz", sessionId: "sess-zz", role: "planner" }), null);
});

test("summarize counts roles, live tabs and working sessions", () => {
  const summary = summarize([
    { role: "planner", live: true, session: { lifecycle: "working" }, removed: false },
    { role: "planner", live: false, session: { lifecycle: "idle" }, removed: false },
    { role: "builder", live: true, session: { agent: "running" }, removed: true },
  ]);
  assert.deepEqual(summary, { roles: 2, liveTabs: 2, sessionsWorking: 2, removed: 1, rows: 3 });
});
