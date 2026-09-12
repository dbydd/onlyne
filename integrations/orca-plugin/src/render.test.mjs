import assert from "node:assert/strict";
import { test } from "node:test";
import { collectBoard } from "./board.mjs";
import {
  BOARD_TEXT_LIMIT,
  EMPTY_BOARD_NOTE,
  formatAgentContext,
  formatBoard,
  relativeTime,
  rowLine,
  shortPaneKey,
  summaryLine,
} from "./render.mjs";
import { fakeOnlyne, fakeOrca, roleRow, sessionRow, tabRow } from "./testing.mjs";

const ROOT_A = "/srv/onlyne-a";
const ROOT_B = "/srv/onlyne-b";
const NOW = Date.parse("2026-09-11T00:10:00Z");

function idleSession(overrides = {}) {
  return sessionRow({
    task_id: "task-beta",
    role: "builder",
    session_id: "sess-beta",
    lifecycle: "idle",
    agent: "gone",
    ...overrides,
  });
}

test("the empty board states what is missing in one line", async () => {
  const board = await collectBoard({ orca: fakeOrca(), onlyne: fakeOnlyne(), serverRoots: [] });
  const text = formatBoard(board, { now: NOW });
  assert.equal(text.split("\n").length, 2);
  assert.match(text, /0 roots · 0 roles · 0 tabs \(0 live\) · 0 sessions \(0 working\)/);
  assert.ok(text.includes(EMPTY_BOARD_NOTE));
});

test("a populated board renders the root, its role sections, and the stray tabs", async () => {
  const board = await collectBoard({
    orca: fakeOrca({
      tabs: [
        tabRow({ lastOutputAt: NOW - 12_000 }),
        tabRow({
          handle: "term_22222222-2222-4222-8222-222222222222",
          tabId: "470e41ba-86b3-43b4-86c3-46c634619a07",
          leafId: "31f6d4b1-7192-4e59-b90b-2e8962d72353",
          title: "Pi ready",
          worktreeId: "53e59790-a6b1-4f50-b424-d5c19b36428a",
          lastOutputAt: NOW - 5_000,
        }),
        tabRow({
          handle: "term_3",
          tabId: "tab-3",
          leafId: "leaf-3",
          title: "zsh",
          connected: false,
          lastOutputAt: NOW - 3 * 60_000,
        }),
      ],
    }),
    onlyne: fakeOnlyne({
      // Every tab is on the axis because a session reports its pane — which is
      // also why the disconnected `zsh` tab still shows: its pi reported from
      // inside it before the tab went down.
      sessions: {
        [ROOT_A]: [
          sessionRow(),
          idleSession({ paneKey: "470e41ba-86b3-43b4-86c3-46c634619a07:31f6d4b1-7192-4e59-b90b-2e8962d72353" }),
          sessionRow({
            task_id: "task-gamma",
            session_id: "sess-gamma",
            lifecycle: "idle",
            agent: "gone",
            paneKey: "tab-3:leaf-3",
          }),
        ],
      },
      roles: { [ROOT_A]: [roleRow(), roleRow({ name: "builder", state: "draining" })] },
    }),
    serverRoots: [ROOT_A],
  });

  const lines = formatBoard(board, { now: NOW }).split("\n");
  assert.match(lines[0], /1 roots · 2 roles · 3 tabs \(2 live\) · 3 sessions \(1 working\)/);
  assert.equal(lines[1], "/srv/onlyne-a  (2 roles · 3 sessions · 1 working)");
  assert.equal(lines[2], "  builder  (draining · 1 tasks · 0 live)");
  assert.equal(lines[3], "    ○ task-bet · idle/gone · 无 tab · 9m · —");
  assert.equal(lines[4], "  planner  (online · 2 tasks · 1 live)");
  assert.equal(lines[5], "    ● task-alp · working/running · 12s · 45e603f7:b6d067b6");
  assert.equal(lines[6], "    ○ task-gam · idle/gone · 无 tab · 9m · —");
  assert.equal(lines[7], "未 join 的 tab (2)");
  assert.equal(lines[8], "  ● tab · title=Pi ready · 5s · 470e41ba:31f6d4b1 · wt 53e59790");
  assert.equal(lines[9], "  ○ tab · title=zsh · 3m · tab-3:leaf-3 · wt 2ea2fe23");
  assert.ok(lines.every((line) => line.length <= BOARD_TEXT_LIMIT));
});

test("a cut tab axis is explained in its own line, and the dropped tab is not rendered", async () => {
  const board = await collectBoard({
    orca: fakeOrca({
      tabs: [tabRow(), tabRow({ handle: "term_other", tabId: "tab-other", leafId: "leaf-other", title: "zsh" })],
    }),
    onlyne: fakeOnlyne({ sessions: { [ROOT_A]: [sessionRow()] } }),
    serverRoots: [ROOT_A],
  });
  const text = formatBoard(board, { now: NOW });
  const lines = text.split("\n");

  assert.match(lines[0], /1 tabs \(1 live\) · 1 hidden/);
  assert.equal(
    lines[1],
    "tab 轴：只列 1 个连着 adapter 的 pi pane（session 上报的 host.orca.pane_key），其余 1 个 tab 不计入"
  );
  assert.equal(lines[2], "/srv/onlyne-a  (1 roles · 1 sessions · 1 working)");
  assert.equal(/zsh/.test(text), false, "a hidden tab is counted, never rendered");
});

test("a board with tabs but no reported pane explains the cut instead of denying the tabs", async () => {
  // The live case the smoke run walks into: Orca lists tabs, no session has
  // reported a pane yet, and no root is configured. The board must not claim
  // there is no Orca tab — there plainly are three.
  const board = await collectBoard({
    orca: fakeOrca({
      tabs: [
        tabRow({ title: "zsh" }),
        tabRow({ handle: "term_2", tabId: "tab-2", leafId: "leaf-2", title: "zsh" }),
        tabRow({ handle: "term_3", tabId: "tab-3", leafId: "leaf-3", title: "zsh" }),
      ],
    }),
    onlyne: fakeOnlyne(),
    serverRoots: [],
  });
  const text = formatBoard(board, { now: NOW });

  assert.equal(text.includes(EMPTY_BOARD_NOTE), false);
  assert.match(text, /0 tabs \(0 live\) · 3 hidden/);
  assert.match(text, /等 pi-onlyne 连上/);
});

test("an unreachable root renders its own failure without blanking the board", async () => {
  const board = await collectBoard({
    orca: fakeOrca({ tabs: [tabRow({ lastOutputAt: NOW - 12_000 })] }),
    onlyne: fakeOnlyne({
      sessions: { [ROOT_A]: [sessionRow()] },
      roles: { [ROOT_A]: [roleRow()] },
      failures: {
        [ROOT_B]: { ok: false, code: "cli_error", message: "onlyne: no onlyne socket found" },
      },
    }),
    serverRoots: [ROOT_A, ROOT_B],
  });

  const text = formatBoard(board, { now: NOW });
  assert.match(text, /● task-alp/);
  assert.match(text, /^\/srv\/onlyne-b {2}\(0 roles · 0 sessions · 0 working\)$/m);
  assert.match(text, /^ {2}! sessions: cli_error — onlyne: no onlyne socket found$/m);
  assert.match(text, /^ {2}! roles: cli_error — onlyne: no onlyne socket found$/m);
});

test("a failed tab axis renders its own single error line", async () => {
  const board = await collectBoard({
    orca: fakeOrca({ tabFailure: { ok: false, code: "missing_binary", message: "binary not found on PATH" } }),
    onlyne: fakeOnlyne({ sessions: { [ROOT_A]: [sessionRow()] } }),
    serverRoots: [ROOT_A],
  });

  const lines = formatBoard(board, { now: NOW }).split("\n");
  assert.equal(lines[1], "! tabs: missing_binary — binary not found on PATH");
  assert.match(lines[2], /^\/srv\/onlyne-a {2}\(1 roles · 1 sessions · 1 working\)$/);
});

test("a long board is truncated with a remainder marker instead of overflowing", async () => {
  const sessions = Array.from({ length: 60 }, (_, index) =>
    sessionRow({ task_id: `task-${String(index).padStart(3, "0")}` })
  );
  const board = await collectBoard({
    orca: fakeOrca(),
    onlyne: fakeOnlyne({ sessions: { [ROOT_A]: sessions } }),
    serverRoots: [ROOT_A],
  });
  const text = formatBoard(board, { now: NOW });
  assert.ok(text.length <= BOARD_TEXT_LIMIT);
  assert.match(text, /…\(\+\d+ 行\)/);
});

test("relative times cover seconds, minutes, hours and days", () => {
  assert.equal(relativeTime(NOW - 12_000, NOW), "12s");
  assert.equal(relativeTime(NOW - 3 * 60_000, NOW), "3m");
  assert.equal(relativeTime(NOW - 5 * 3_600_000, NOW), "5h");
  assert.equal(relativeTime(NOW - 49 * 3_600_000, NOW), "2d");
  assert.equal(relativeTime("2026-09-11T00:09:30Z", NOW), "30s");
  assert.equal(relativeTime(null, NOW), "—");
});

test("pane keys render in a short, stable form", () => {
  assert.equal(shortPaneKey("45e603f7-0772-48aa-bcf6-832272747713:b6d067b6-9255-4f5c-a13f-24f194ea0560"), "45e603f7:b6d067b6");
  assert.equal(shortPaneKey("odd"), "odd");
  assert.equal(shortPaneKey(null), "—");
});

test("a task row without a tab keeps the state column and dashes the pane", () => {
  const line = rowLine(
    {
      kind: "task",
      taskId: "task-alpha",
      joined: false,
      live: false,
      paneKey: null,
      lastOutputAt: null,
      session: { lifecycle: "idle", agent: "gone", updatedAt: "2026-09-11T00:05:00Z" },
    },
    NOW
  );
  assert.equal(line, "○ task-alp · idle/gone · 无 tab · 5m · —");
});

test("summaryLine is a single compact line", () => {
  assert.equal(
    summaryLine({ summary: { roots: 2, roles: 3, tabs: 9, liveTabs: 4, sessions: 7, sessionsWorking: 1 } }),
    "Onlyne sessions · 2 roots · 3 roles · 9 tabs (4 live) · 7 sessions (1 working)"
  );
});

test("agent context carries the triple and omits what a row does not have", () => {
  const joined = formatAgentContext({
    taskId: "task-alpha",
    paneKey: "45e603f7-0772-48aa-bcf6-832272747713:b6d067b6-9255-4f5c-a13f-24f194ea0560",
    handle: "term_x",
    selector: "2ea2fe23-829c-4a8f-bcac-4129eb78a164",
  });
  assert.deepEqual(joined.split("\n"), [
    "task: task-alpha",
    "pane_key: 45e603f7-0772-48aa-bcf6-832272747713:b6d067b6-9255-4f5c-a13f-24f194ea0560",
    "handle: term_x",
    "orca selector: 2ea2fe23-829c-4a8f-bcac-4129eb78a164",
  ]);
  assert.equal(
    formatAgentContext({ taskId: null, paneKey: null, handle: "term_y", selector: null }),
    "pane_key: —\nhandle: term_y"
  );
});
