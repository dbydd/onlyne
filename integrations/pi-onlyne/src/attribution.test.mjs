// The pane claim is the interface between pi and the supervisor board
// (integrations/orca-plugin): pi states which Orca pane it runs in, the board
// filters its tab axis by that statement. These tests pin the file it produces,
// the fact that a broken environment produces no claim rather than a failure,
// and that clearing is a real removal.

import assert from "node:assert/strict";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, test } from "node:test";

import { PANE_CLAIM_RELATIVE_PATH, paneClaim, publishPaneClaim } from "./attribution.mjs";

const TAB_ID = "45e603f7-0772-48aa-bcf6-832272747713";
const LEAF_ID = "b6d067b6-9255-4f5c-a13f-24f194ea0560";
const PANE_KEY = `${TAB_ID}:${LEAF_ID}`;
const PANE_ENV = {
  ORCA_PANE_KEY: PANE_KEY,
  ORCA_TAB_ID: TAB_ID,
  ORCA_TERMINAL_HANDLE: "term_11111111-1111-4111-8111-111111111111",
  ORCA_WORKTREE_ID: "2ea2fe23-829c-4a8f-bcac-4129eb78a164",
};

const dirs = [];
afterEach(() => {
  while (dirs.length > 0) rmSync(dirs.pop(), { recursive: true, force: true });
});

function workspace() {
  const dir = mkdtempSync(join(tmpdir(), "pi-onlyne-claim-"));
  dirs.push(dir);
  return dir;
}

test("a pane claim names the pane the process was spawned in", () => {
  assert.deepEqual(paneClaim(PANE_ENV, { role: "planner", taskId: "task-alpha" }), {
    pane_key: PANE_KEY,
    tab_id: TAB_ID,
    leaf_id: LEAF_ID,
    handle: PANE_ENV.ORCA_TERMINAL_HANDLE,
    worktree_id: PANE_ENV.ORCA_WORKTREE_ID,
    role: "planner",
    task_id: "task-alpha",
  });

  // The pane key is what the board attributes the tab by, so a claim survives
  // without a role, a task or the optional ids, and only the pane key can veto.
  assert.deepEqual(paneClaim({ ORCA_PANE_KEY: PANE_KEY }), {
    pane_key: PANE_KEY,
    tab_id: TAB_ID,
    leaf_id: LEAF_ID,
    handle: null,
    worktree_id: null,
    role: null,
    task_id: null,
  });
  assert.equal(paneClaim({}), null, "a plain shell is not in a pane");
  assert.equal(paneClaim({ ORCA_PANE_KEY: "not-a-pane-key" }), null);
  assert.equal(paneClaim(undefined), null);
});

test("publishing writes the claim, and clearing removes it", () => {
  const dir = workspace();
  const claim = paneClaim(PANE_ENV, { role: "planner", taskId: "task-alpha" });

  const written = publishPaneClaim({ workspace: dir, claim });
  assert.equal(written.written, true);
  assert.equal(written.path, join(dir, PANE_CLAIM_RELATIVE_PATH));
  assert.deepEqual(JSON.parse(readFileSync(written.path, "utf8")), claim);

  // Clearing is a removal, not an empty file: the board reads "no file" as
  // "no claim" and an empty one would be a parse failure it has to tolerate.
  const cleared = publishPaneClaim({ workspace: dir, claim: null });
  assert.equal(cleared.written, false);
  assert.equal(existsSync(cleared.path), false);
  publishPaneClaim({ workspace: dir, claim: null }); // already gone: still quiet
});

test("a claim replaces an older one instead of merging with it", () => {
  const dir = workspace();
  const path = publishPaneClaim({ workspace: dir, claim: paneClaim(PANE_ENV, { role: "planner" }) }).path;
  const other = `${TAB_ID}:aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee`;
  publishPaneClaim({ workspace: dir, claim: paneClaim({ ...PANE_ENV, ORCA_PANE_KEY: other }, { role: "builder" }) });

  const reread = JSON.parse(readFileSync(path, "utf8"));
  assert.equal(reread.pane_key, other);
  assert.equal(reread.role, "builder");
  assert.equal(reread.leaf_id, "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee");
});

test("no workspace or an unwritable one is a quiet no-op", () => {
  assert.deepEqual(publishPaneClaim({ workspace: "", claim: { pane_key: PANE_KEY } }), {
    written: false,
    path: null,
  });

  // A file where the cache directory should be: the write fails, the session
  // does not.
  const dir = workspace();
  writeFileSync(join(dir, ".onlyne"), "not a directory", "utf8");
  const failed = publishPaneClaim({ workspace: dir, claim: { pane_key: PANE_KEY } });
  assert.equal(failed.written, false);
  assert.equal(existsSync(join(dir, ".onlyne")), true, "the obstacle is left alone");
});

test("the claim is written through a temporary file and renamed", () => {
  const dir = workspace();
  mkdirSync(join(dir, ".onlyne", "cache"), { recursive: true });
  const renames = [];
  publishPaneClaim({
    workspace: dir,
    claim: { pane_key: PANE_KEY },
    fs: {
      mkdirSync,
      writeFileSync,
      rmSync,
      renameSync: (from, to) => {
        renames.push({ from, to });
        return writeFileSync(to, readFileSync(from));
      },
    },
  });

  assert.equal(renames.length, 1);
  assert.equal(renames[0].from, `${join(dir, PANE_CLAIM_RELATIVE_PATH)}.${process.pid}.tmp`);
  assert.equal(renames[0].to, join(dir, PANE_CLAIM_RELATIVE_PATH));
});
