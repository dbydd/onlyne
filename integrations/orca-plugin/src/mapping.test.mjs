import assert from "node:assert/strict";
import { after, test } from "node:test";
import { join } from "node:path";
import { MAPPING_RELATIVE_PATH, mappingPath, parseMapping, readMapping } from "./mapping.mjs";
import { cleanupAll, makeWorkspace, mappingRow } from "./testing.mjs";

after(cleanupAll);

test("later lines override earlier ones for the same pane_key", () => {
  const parsed = parseMapping(
    [
      mappingRow({ state: "spawned", task_id: "task-alpha" }),
      mappingRow({ state: "spawned", task_id: "task-beta", session_id: "sess-beta" }),
    ]
      .map((row) => JSON.stringify(row))
      .join("\n")
  );
  assert.equal(parsed.rows.length, 1);
  assert.equal(parsed.rows[0].taskId, "task-beta");
  assert.equal(parsed.rows[0].sessionId, "sess-beta");
});

test("a closed line is a tombstone and never a live row", () => {
  const tombstone = mappingRow({ pane_key: "tab-9:leaf-9", state: "closed", task_id: "task-dead" });
  const parsed = parseMapping([JSON.stringify(mappingRow()), JSON.stringify(tombstone)].join("\n"));
  assert.deepEqual(
    parsed.rows.map((row) => row.paneKey),
    ["tab-1:leaf-1"]
  );
  assert.deepEqual(
    parsed.tombstones.map((row) => row.paneKey),
    ["tab-9:leaf-9"]
  );
  assert.equal(parsed.tombstones[0].closed, true);
});

test("a tombstone override wins over an earlier spawn for the same pane", () => {
  const parsed = parseMapping(
    [JSON.stringify(mappingRow()), JSON.stringify(mappingRow({ state: "closed" }))].join("\n")
  );
  assert.equal(parsed.rows.length, 0);
  assert.equal(parsed.tombstones.length, 1);
});

test("blank lines are skipped and malformed lines are counted, not fatal", () => {
  const parsed = parseMapping(
    ["", "{ not json", JSON.stringify({ handle: "term_x" }), JSON.stringify(mappingRow()), ""].join("\n")
  );
  assert.equal(parsed.rows.length, 1);
  assert.equal(parsed.malformed, 2);
});

test("a missing mapping file is a normal state, not an error", () => {
  const result = readMapping("/nonexistent/role-workspace");
  assert.equal(result.ok, false);
  assert.equal(result.missing, true);
  assert.deepEqual(result.rows, []);
});

test("reads a real two-line file with a tombstone from disk", () => {
  const workspace = makeWorkspace({
    name: "planner",
    lines: [
      mappingRow(),
      mappingRow({
        pane_key: "tab-2:leaf-2",
        handle: "term_22222222-2222-4222-8222-222222222222",
        task_id: "task-gamma",
        role: "builder",
        state: "closed",
      }),
    ],
  });
  const result = readMapping(workspace);
  assert.equal(result.ok, true);
  assert.equal(result.path, join(workspace, MAPPING_RELATIVE_PATH));
  assert.equal(result.rows.length, 1);
  assert.equal(result.tombstones.length, 1);
  assert.equal(result.rows[0].role, "planner");
  assert.equal(result.rows[0].worktreeSelector, "path:/tmp/role");
});

test("a read failure on an existing file surfaces as an error", () => {
  const workspace = makeWorkspace({ name: "io-error", lines: [mappingRow()] });
  const result = readMapping(workspace, {
    exists: () => true,
    readFile: () => {
      throw new Error("EACCES");
    },
  });
  assert.equal(result.ok, false);
  assert.equal(result.missing, false);
  assert.equal(result.error, "EACCES");
  assert.equal(mappingPath(workspace).endsWith(MAPPING_RELATIVE_PATH), true);
});

test("an empty mapping file yields no rows and no tombstones", () => {
  const workspace = makeWorkspace({ name: "empty", lines: [] });
  const result = readMapping(workspace);
  assert.equal(result.ok, true);
  assert.deepEqual(result.rows, []);
  assert.deepEqual(result.tombstones, []);
  assert.equal(result.malformed, 0);
});
