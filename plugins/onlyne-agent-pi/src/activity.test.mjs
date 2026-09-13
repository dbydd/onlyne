import assert from "node:assert/strict";
import { test } from "node:test";

import { createActivity, MAX_LINES, MAX_TEXT, MAX_WIDTH, SHOW_EVENTS } from "./activity.mjs";

const clock = () => new Date("2026-09-13T09:08:07");

test("header renders before welcome and after state updates", () => {
  const activity = createActivity({ clock }).set({ role: "planner" });
  assert.deepEqual(activity.lines(), ["onlyne planner · connecting"]);

  activity.set({ connection: "connected", generation: 3, taskId: "abcdef012345", phase: "running" });
  assert.equal(activity.lines()[0], "onlyne planner · connected · gen 3 · task abcdef01 running");
});

test("task tail is absent with no task", () => {
  const activity = createActivity({ clock }).set({ role: "planner", connection: "connected", generation: 2, phase: "running" });
  assert.equal(activity.lines()[0], "onlyne planner · connected · gen 2");
});

test("matching events merge and new text gets a new row", () => {
  const activity = createActivity({ clock });
  activity.note("warn", "x").note("warn", "x");
  assert.equal(activity.events.length, 1);
  assert.equal(activity.events[0].repeats, 1);
  assert.match(activity.lines()[1], /09:08:07  !! x x2$/);

  activity.note("warn", "y");
  assert.equal(activity.events.length, 2);
  assert.match(activity.lines()[1], /09:08:07  !! y$/);
});

test("long event text is cut at MAX_TEXT", () => {
  const activity = createActivity({ clock });
  activity.note("state", "a".repeat(MAX_TEXT + 10));
  assert.equal(Array.from(activity.events[0].text).length, MAX_TEXT + 1);
  assert.ok(activity.events[0].text.endsWith("…"));
});

test("hostile input stays inside render bounds", () => {
  const activity = createActivity({ maxEvents: 100, clock }).set({
    role: "r".repeat(48),
    connection: "connected",
    generation: 999,
    taskId: "1234567890abcdef",
    phase: "running",
  });
  for (let index = 0; index < 100; index += 1) activity.note("warn", `${index} ${"x".repeat(5_000)}`);

  const lines = activity.lines();
  assert.ok(lines.length <= MAX_LINES, `line count ${lines.length}`);
  for (const line of lines) assert.ok(Array.from(line).length <= MAX_WIDTH, line);
});

test("identical state gives identical output", () => {
  const activity = createActivity({ clock }).set({ role: "planner", connection: "connected" });
  activity.note("in", "build it");
  assert.deepEqual(activity.lines(), activity.lines());
});

test("default render shows the newest SHOW_EVENTS events", () => {
  const activity = createActivity({ clock });
  for (let index = 0; index < SHOW_EVENTS + 4; index += 1) activity.note("state", `event ${index}`);

  const lines = activity.lines();
  assert.equal(lines.length, SHOW_EVENTS + 1);
  assert.match(lines[1], /event 9$/);
  assert.match(lines.at(-1), new RegExp(`event ${SHOW_EVENTS + 4 - SHOW_EVENTS}$`));
});
