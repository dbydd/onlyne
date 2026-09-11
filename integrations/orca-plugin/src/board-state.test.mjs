import assert from "node:assert/strict";
import { test } from "node:test";
import { createBoardState, structuralFingerprint } from "./board-state.mjs";
import { collectNotifications } from "./testing.mjs";

const NOW = 1_789_000_000_000;

function board(rows) {
  const groups = new Map();
  for (const item of rows) {
    const bucket = groups.get(item.role) ?? { role: item.role, rows: [] };
    bucket.rows.push(item);
    groups.set(item.role, bucket);
  }
  return {
    scannedAt: NOW,
    ok: true,
    errors: [],
    workspaces: [],
    rows,
    groups: [...groups.values()],
    summary: {
      roles: groups.size,
      liveTabs: rows.filter((item) => item.live).length,
      sessionsWorking: 0,
      removed: 0,
      rows: rows.length,
    },
  };
}

function row(overrides = {}) {
  return { role: "planner", taskId: "task-alpha", paneKey: "pane-1", live: true, removed: false, ...overrides };
}

function fakeTimers() {
  const scheduled = [];
  return {
    scheduled,
    timers: {
      setTimeout: (handler, ms) => {
        const handle = { handler, ms, kind: "timeout", unref() {} };
        scheduled.push(handle);
        return handle;
      },
      clearTimeout: (handle) => {
        handle.cleared = true;
      },
      setInterval: (handler, ms) => {
        const handle = { handler, ms, kind: "interval", unref() {} };
        scheduled.push(handle);
        return handle;
      },
      clearInterval: (handle) => {
        handle.cleared = true;
      },
    },
  };
}

function setup({ boards }) {
  const { sent, notify } = collectNotifications();
  const clocks = fakeTimers();
  const logs = [];
  let index = 0;
  const state = createBoardState({
    collect: async () => boards[Math.min(index++, boards.length - 1)],
    notify,
    log: (line) => logs.push(line),
    now: () => NOW,
    timers: clocks.timers,
  });
  return { state, sent, logs, clocks, notify };
}

test("the first scan is quiet and only structural changes notify", async () => {
  const { state, sent } = setup({ boards: [board([row()]), board([row()]), board([row({ taskId: "task-beta" })])] });
  await state.refresh({ reason: "activate" });
  assert.deepEqual(sent, [], "first scan must not notify");
  await state.refresh({ reason: "cadence" });
  assert.deepEqual(sent, [], "an unchanged board must not notify");
  await state.refresh({ reason: "cadence" });
  assert.equal(sent.length, 1);
  assert.match(sent[0].body, /task-bet/);
  state.stop();
});

test("session churn alone does not notify, structure does", async () => {
  const withSession = (lifecycle) => {
    const item = row();
    item.session = { lifecycle, agent: "running" };
    return board([item]);
  };
  const { state, sent } = setup({ boards: [withSession("idle"), withSession("working"), withSession("working")] });
  await state.refresh({ reason: "activate" });
  await state.refresh({ reason: "cadence" });
  assert.deepEqual(sent, [], "a lifecycle flip inside one row is not a structural change");
  state.stop();
});

test("notifications are rate limited after the first structural change", async () => {
  const { state, sent } = setup({
    boards: [board([row()]), board([row({ taskId: "task-beta" })]), board([row({ taskId: "task-gamma" })])],
  });
  await state.refresh({ reason: "activate" });
  await state.refresh({ reason: "one" });
  await state.refresh({ reason: "two" });
  assert.equal(sent.length, 1, "the second change lands inside the cooldown");
  state.stop();
});

test("event rescans are debounced into a single scan", async () => {
  const { state, clocks } = setup({ boards: [board([row()])] });
  state.scheduleRefresh({ reason: "worktree.created" });
  state.scheduleRefresh({ reason: "agent.status.changed" });
  const timeouts = clocks.scheduled.filter((handle) => handle.kind === "timeout");
  assert.equal(timeouts.length, 2);
  assert.equal(timeouts[0].cleared, true, "the first timer is cancelled by the second");
  await timeouts[1].handler();
  state.stop();
});

test("start schedules the fallback cadence and stop clears it", () => {
  const { state, clocks } = setup({ boards: [board([row()])] });
  state.start();
  const interval = clocks.scheduled.find((handle) => handle.kind === "interval");
  assert.ok(interval);
  assert.equal(interval.ms, 5000);
  state.stop();
  assert.equal(interval.cleared, true);
});

test("a failed scan keeps the previous board instead of blanking it", async () => {
  const { sent, notify } = collectNotifications();
  const logs = [];
  const previous = board([row()]);
  let calls = 0;
  const state = createBoardState({
    collect: async () => {
      calls += 1;
      if (calls === 1) return previous;
      throw new Error("orca exploded");
    },
    notify,
    log: (line) => logs.push(line),
    now: () => NOW,
    timers: fakeTimers().timers,
  });
  await state.refresh({ reason: "activate" });
  const after = await state.refresh({ reason: "cadence" });
  assert.equal(after, previous);
  assert.equal(state.getBoard(), previous);
  assert.ok(logs.some((line) => line.includes("orca exploded")));
  assert.deepEqual(sent, []);
  state.stop();
});

test("structuralFingerprint ignores array order but not content", () => {
  const a = board([row({ taskId: "a" }), row({ taskId: "b" })]);
  const b = board([row({ taskId: "b" }), row({ taskId: "a" })]);
  const c = board([row({ taskId: "a" })]);
  assert.equal(structuralFingerprint(a), structuralFingerprint(b));
  assert.notEqual(structuralFingerprint(a), structuralFingerprint(c));
  assert.equal(structuralFingerprint(null), "none");
});
