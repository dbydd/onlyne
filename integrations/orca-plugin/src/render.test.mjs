import assert from "node:assert/strict";
import { test } from "node:test";
import {
  BOARD_TEXT_LIMIT,
  EMPTY_BOARD_NOTE,
  formatAgentContext,
  formatBoard,
  relativeTime,
  shortPaneKey,
  summaryLine,
} from "./render.mjs";

const NOW = Date.parse("2026-09-11T00:10:00Z");

function row(overrides = {}) {
  return {
    role: "planner",
    taskId: "task-alpha",
    paneKey: "45e603f7-0772-48aa-bcf6-832272747713:b6d067b6-9255-4f5c-a13f-24f194ea0560",
    handle: "term_x",
    selector: "path:/tmp/planner",
    mappingState: "spawned",
    removed: false,
    live: true,
    terminal: { connected: true, lastOutputAt: NOW - 12_000 },
    session: { lifecycle: "working", agent: "running" },
    ...overrides,
  };
}

function board(rows) {
  const groups = new Map();
  for (const item of rows) {
    const bucket = groups.get(item.role) ?? { role: item.role, rows: [] };
    bucket.rows.push(item);
    groups.set(item.role, bucket);
  }
  const list = [...groups.values()].map((group) => ({
    ...group,
    liveTabs: group.rows.filter((item) => item.live).length,
    removed: group.rows.every((item) => item.removed),
  }));
  return {
    scannedAt: NOW,
    rows,
    groups: list,
    summary: {
      roles: list.length,
      liveTabs: rows.filter((item) => item.live).length,
      sessionsWorking: rows.filter((item) => item.session?.lifecycle === "working").length,
      removed: rows.filter((item) => item.removed).length,
      rows: rows.length,
    },
  };
}

test("the empty board states the wait-for-supervisor case in one line", () => {
  const text = formatBoard(board([]), { now: NOW });
  assert.equal(text.split("\n").length, 2);
  assert.match(text, /0 roles · 0 live tabs · 0 working/);
  assert.ok(text.includes(EMPTY_BOARD_NOTE));
});

test("a populated board groups rows by role and marks liveness", () => {
  const text = formatBoard(
    board([
      row(),
      row({
        taskId: "task-beta",
        paneKey: "abcdef01-2345-6789-abcd-ef0123456789:12345678-9999-9999-9999-999999999999",
        live: false,
        terminal: null,
        session: null,
        mappingState: "spawned",
      }),
      row({ role: "builder", taskId: "task-gamma", removed: true, live: false, terminal: null }),
    ]),
    { now: NOW }
  );
  const lines = text.split("\n");
  assert.match(lines[0], /2 roles · 1 live tabs · 2 working/);
  assert.ok(lines.some((line) => line.startsWith("builder")));
  assert.ok(lines.some((line) => line.includes("● task-alp") && line.includes("working/running") && line.includes("12s")));
  assert.ok(lines.some((line) => line.includes("○ task-bet")));
  assert.ok(lines.some((line) => line.includes("✕ task-gam") && line.includes("worktree 已移除")));
  assert.ok(lines.every((line) => line.length <= BOARD_TEXT_LIMIT));
});

test("a long board is truncated with a remainder marker instead of overflowing", () => {
  const rows = Array.from({ length: 60 }, (_, index) =>
    row({ taskId: `task-${String(index).padStart(3, "0")}`, paneKey: `pane-${index}` })
  );
  const text = formatBoard(board(rows), { now: NOW });
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

test("summaryLine is a single compact line", () => {
  assert.equal(
    summaryLine({ summary: { roles: 2, liveTabs: 3, sessionsWorking: 1 } }),
    "Onlyne sessions · 2 roles · 3 live tabs · 1 working"
  );
});

test("agent context text carries the three addressing strings", () => {
  const text = formatAgentContext(row());
  assert.match(text, /pane_key: 45e603f7/);
  assert.match(text, /handle: term_x/);
  assert.match(text, /orca selector: path:\/tmp\/planner/);
});
