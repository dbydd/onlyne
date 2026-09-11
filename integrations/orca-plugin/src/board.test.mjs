import assert from "node:assert/strict";
import { test } from "node:test";
import { UNKNOWN_ROLE, collectBoard, indexTabsByTask, isWorking, taskIdFromTitle } from "./board.mjs";
import { fakeOnlyne, fakeOrca, roleRow, sessionRow, tabRow } from "./testing.mjs";

const ROOT_A = "/srv/onlyne-a";
const ROOT_B = "/srv/onlyne-b";
const NO_SOCKET = {
  ok: false,
  code: "cli_error",
  message: "onlyne: no onlyne socket found; pass --socket, --server-root, or --workspace",
};

/** The task row the board produced for one root/role, if any. */
function taskRowOf(board, index = 0) {
  return board.roots[index].groups.flatMap((group) => group.rows)[0];
}

test("a tab whose trimmed title is onlyne:<task_id> joins that task's row", async () => {
  const board = await collectBoard({
    orca: fakeOrca({ tabs: [tabRow({ title: "  onlyne:task-alpha  " })] }),
    onlyne: fakeOnlyne({ sessions: { [ROOT_A]: [sessionRow()] }, roles: { [ROOT_A]: [roleRow()] } }),
    serverRoots: [ROOT_A],
  });

  assert.equal(board.ok, true);
  assert.deepEqual(board.errors, []);
  assert.equal(board.rows.length, 1);
  const [row] = board.rows;
  assert.equal(row.kind, "task");
  assert.equal(row.root, ROOT_A);
  assert.equal(row.taskId, "task-alpha");
  assert.equal(row.role, "planner");
  assert.equal(row.joined, true);
  assert.equal(row.live, true);
  assert.equal(row.handle, "term_11111111-1111-4111-8111-111111111111");
  assert.equal(row.paneKey, "45e603f7-0772-48aa-bcf6-832272747713:b6d067b6-9255-4f5c-a13f-24f194ea0560");
  assert.equal(row.selector, "2ea2fe23-829c-4a8f-bcac-4129eb78a164");
  assert.equal(row.lastOutputAt, 1_789_000_000_000);
  assert.deepEqual(board.strayTabs, []);
  assert.deepEqual(board.summary, {
    roots: 1,
    rootsFailed: 0,
    roles: 1,
    tabs: 1,
    liveTabs: 1,
    sessions: 1,
    sessionsWorking: 1,
    joined: 1,
    strayTabs: 0,
    rows: 1,
  });
});

test("the title join is exact: an extra word steals nothing", async () => {
  const board = await collectBoard({
    orca: fakeOrca({
      tabs: [
        tabRow({
          title: "onlyne:task-alpha extra",
          tabId: "aaaa1111-1111-4111-8111-111111111111",
          leafId: "bbbb2222-2222-4222-8222-222222222222",
          connected: false,
        }),
      ],
    }),
    onlyne: fakeOnlyne({ sessions: { [ROOT_A]: [sessionRow()] } }),
    serverRoots: [ROOT_A],
  });

  const row = taskRowOf(board);
  assert.equal(row.joined, false);
  assert.equal(row.handle, null);
  assert.equal(row.paneKey, null);
  assert.equal(row.live, false);
  assert.equal(board.strayTabs.length, 1);
  assert.equal(board.strayTabs[0].paneKey, "aaaa1111-1111-4111-8111-111111111111:bbbb2222-2222-4222-8222-222222222222");
  assert.equal(board.strayTabs[0].live, false);
  assert.equal(board.summary.joined, 0);
  assert.equal(board.summary.strayTabs, 1);
});

test("a tab that joins no session renders as its own row", async () => {
  const board = await collectBoard({
    orca: fakeOrca({
      tabs: [
        tabRow({
          title: "Pi ready",
          tabId: "470e41ba-86b3-43b4-86c3-46c634619a07",
          leafId: "31f6d4b1-7192-4e59-b90b-2e8962d72353",
          worktreeId: "53e59790-a6b1-4f50-b424-d5c19b36428a",
        }),
      ],
    }),
    onlyne: fakeOnlyne({ sessions: { [ROOT_A]: [sessionRow({ task_id: "task-beta" })] } }),
    serverRoots: [ROOT_A],
  });

  assert.equal(board.strayTabs.length, 1);
  const [stray] = board.strayTabs;
  assert.equal(stray.kind, "tab");
  assert.equal(stray.root, null);
  assert.equal(stray.role, null);
  assert.equal(stray.live, true);
  assert.equal(stray.title, "Pi ready");
  assert.equal(stray.worktreeId, "53e59790-a6b1-4f50-b424-d5c19b36428a");
  assert.equal(stray.selector, "53e59790-a6b1-4f50-b424-d5c19b36428a");
  assert.equal(taskRowOf(board).joined, false);
});

test("two tabs carrying one title: the first joins, the second stays standalone", async () => {
  const board = await collectBoard({
    orca: fakeOrca({
      tabs: [
        tabRow(),
        tabRow({ handle: "term_22222222-2222-4222-8222-222222222222", tabId: "tab-2", leafId: "leaf-2" }),
      ],
    }),
    onlyne: fakeOnlyne({ sessions: { [ROOT_A]: [sessionRow()] } }),
    serverRoots: [ROOT_A],
  });

  assert.equal(taskRowOf(board).handle, "term_11111111-1111-4111-8111-111111111111");
  assert.deepEqual(
    board.strayTabs.map((row) => row.handle),
    ["term_22222222-2222-4222-8222-222222222222"]
  );
  assert.equal(board.summary.joined, 1);
});

test("one task id on two roots: the earlier root in config order takes the tab", async () => {
  const board = await collectBoard({
    orca: fakeOrca({ tabs: [tabRow()] }),
    onlyne: fakeOnlyne({
      sessions: { [ROOT_A]: [sessionRow()], [ROOT_B]: [sessionRow({ session_id: "sess-b" })] },
      roles: { [ROOT_A]: [roleRow()], [ROOT_B]: [roleRow({ state: "offline" })] },
    }),
    serverRoots: [ROOT_A, ROOT_B],
  });

  assert.equal(taskRowOf(board, 0).joined, true);
  const later = taskRowOf(board, 1);
  assert.equal(later.taskId, "task-alpha");
  assert.equal(later.joined, false);
  assert.equal(later.handle, null);
  assert.equal(board.summary.joined, 1);
  assert.equal(board.summary.liveTabs, 1);
  assert.deepEqual(board.strayTabs, []);
});

test("an unreachable root reports its own failure while the rest of the board renders", async () => {
  const board = await collectBoard({
    orca: fakeOrca({ tabs: [tabRow()] }),
    onlyne: fakeOnlyne({
      sessions: { [ROOT_A]: [sessionRow()] },
      roles: { [ROOT_A]: [roleRow()] },
      failures: { [ROOT_B]: NO_SOCKET },
    }),
    serverRoots: [ROOT_A, ROOT_B],
  });

  assert.equal(board.ok, true, "a dead root is not a board failure");
  assert.equal(board.summary.rootsFailed, 1);
  assert.deepEqual(
    board.errors.map((error) => `${error.scope} ${error.axis} ${error.code}`),
    [`${ROOT_B} sessions cli_error`, `${ROOT_B} roles cli_error`]
  );
  assert.match(board.errors[0].message, /no onlyne socket found/);
  assert.deepEqual(board.roots[1].groups, []);
  assert.equal(board.roots[1].failures.length, 2);
  assert.equal(taskRowOf(board, 0).joined, true);
});

test("zero serverRoots is valid: flat tabs, no error, no onlyne call", async () => {
  const onlyne = fakeOnlyne();
  const board = await collectBoard({ orca: fakeOrca({ tabs: [tabRow(), tabRow({ handle: "term_2", tabId: "tab-2", leafId: "leaf-2", title: "zsh" })] }), onlyne, serverRoots: [] });

  assert.equal(board.ok, true);
  assert.deepEqual(board.errors, []);
  assert.deepEqual(board.roots, []);
  assert.equal(board.strayTabs.length, 2);
  assert.deepEqual(onlyne.calls, []);
  assert.deepEqual(board.summary, {
    roots: 0,
    rootsFailed: 0,
    roles: 0,
    tabs: 2,
    liveTabs: 2,
    sessions: 0,
    sessionsWorking: 0,
    joined: 0,
    strayTabs: 2,
    rows: 2,
  });
});

test("a failing tab axis still shows the session axis and keeps its error code", async () => {
  const board = await collectBoard({
    orca: fakeOrca({ tabFailure: { ok: false, code: "missing_binary", message: "binary not found on PATH" } }),
    onlyne: fakeOnlyne({ sessions: { [ROOT_A]: [sessionRow()] }, roles: { [ROOT_A]: [roleRow()] } }),
    serverRoots: [ROOT_A],
  });

  assert.equal(board.ok, false);
  assert.deepEqual(board.tabs, []);
  assert.deepEqual(board.errors, [
    { scope: "orca", axis: "tabs", code: "missing_binary", message: "binary not found on PATH" },
  ]);
  const row = taskRowOf(board);
  assert.equal(row.taskId, "task-alpha");
  assert.equal(row.joined, false);
  assert.equal(row.live, false);
});

test("a role with no sessions still owns a section, and an unknown role gets one", async () => {
  const board = await collectBoard({
    orca: fakeOrca(),
    onlyne: fakeOnlyne({
      sessions: { [ROOT_A]: [sessionRow({ role: null })] },
      roles: { [ROOT_A]: [roleRow({ name: "builder", state: "offline", sessions: 0 })] },
    }),
    serverRoots: [ROOT_A],
  });

  assert.deepEqual(
    board.roots[0].groups.map((group) => group.role),
    ["builder", UNKNOWN_ROLE]
  );
  assert.deepEqual(board.roots[0].groups[0].rows, []);
  assert.equal(board.roots[0].summary.sessions, 1);
  assert.equal(board.summary.roles, 2);
});

test("a legacy onlyne binary degrades one verb without dropping the root", async () => {
  const board = await collectBoard({
    orca: fakeOrca(),
    onlyne: fakeOnlyne({
      roles: { [ROOT_A]: [roleRow()] },
      failures: {
        [`sessions ${ROOT_A}`]: {
          ok: false,
          code: "cli_surface_mismatch",
          message: "error: unexpected argument '--server-root' found",
        },
      },
    }),
    serverRoots: [ROOT_A],
  });

  assert.equal(board.roots[0].sessionsScan.ok, false);
  assert.equal(board.roots[0].sessionsScan.code, "cli_surface_mismatch");
  assert.equal(board.roots[0].rolesScan.ok, true);
  assert.deepEqual(
    board.roots[0].groups.map((group) => `${group.role}:${group.rows.length}`),
    ["planner:0"]
  );
});

test("taskIdFromTitle accepts only the exact prefix", () => {
  assert.equal(taskIdFromTitle("onlyne:abc"), "abc");
  assert.equal(taskIdFromTitle("  onlyne:abc  "), "abc");
  assert.equal(taskIdFromTitle("onlyne:"), null);
  assert.equal(taskIdFromTitle("onlyne abc"), null);
  assert.equal(taskIdFromTitle("onlyne:abc extra"), "abc extra");
  assert.equal(taskIdFromTitle(null), null);
});

test("indexTabsByTask groups duplicate titles in Orca's own order", () => {
  const index = indexTabsByTask([tabRow(), tabRow({ handle: "term_2" }), tabRow({ title: "zsh" })]);
  assert.deepEqual(
    index.get("task-alpha").map((tab) => tab.handle),
    ["term_11111111-1111-4111-8111-111111111111", "term_2"]
  );
  assert.equal(index.size, 1);
});

test("isWorking follows the session lifecycle, then the agent", () => {
  assert.equal(isWorking({ session: { lifecycle: "working" } }), true);
  assert.equal(isWorking({ session: { lifecycle: "idle", agent: "running" } }), true);
  assert.equal(isWorking({ session: { lifecycle: "exited", agent: "gone" } }), false);
  assert.equal(isWorking({ session: null }), false);
});
