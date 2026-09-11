// The pane claim is the interface between pi and the supervisor board
// (integrations/orca-plugin): pi states which Orca pane it runs in, the board
// filters its tab axis by that statement. These tests pin the file it produces —
// one per pane, never one for a workspace — the fact that a broken environment
// produces no claim rather than a failure, and that clearing removes this pane's
// claim and nothing else.

import assert from "node:assert/strict";
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, test } from "node:test";

import {
  PANE_CLAIMS_RELATIVE_DIR,
  paneClaim,
  paneClaimFileName,
  publishPaneClaim,
} from "./attribution.mjs";

const TAB_ID = "45e603f7-0772-48aa-bcf6-832272747713";
const LEAF_ID = "b6d067b6-9255-4f5c-a13f-24f194ea0560";
const PANE_KEY = `${TAB_ID}:${LEAF_ID}`;
const OTHER_LEAF = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
const OTHER_KEY = `${TAB_ID}:${OTHER_LEAF}`;
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

/** Where one pane's claim lands inside a workspace. */
function claimPath(dir, paneKey) {
  return join(dir, PANE_CLAIMS_RELATIVE_DIR, paneClaimFileName(paneKey));
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

test("a claim file is the pane key with its colon flattened, and nothing else", () => {
  assert.equal(paneClaimFileName(PANE_KEY), `${TAB_ID}-${LEAF_ID}.json`);
  assert.equal(paneClaimFileName(OTHER_KEY), `${TAB_ID}-${OTHER_LEAF}.json`);

  // The key is inherited from the environment, so it is never trusted as a path
  // segment: anything that would leave the directory is not a file name here.
  assert.equal(paneClaimFileName("../escape:leaf"), null);
  assert.equal(paneClaimFileName("nested/key:leaf"), null);
  assert.equal(paneClaimFileName(""), null);
  assert.equal(paneClaimFileName(null), null);
});

test("publishing writes the claim, and clearing removes it", () => {
  const dir = workspace();
  const claim = paneClaim(PANE_ENV, { role: "planner", taskId: "task-alpha" });

  const written = publishPaneClaim({ workspace: dir, claim, now: () => "2026-09-11T04:05:06.789Z" });
  assert.equal(written.written, true);
  assert.equal(written.path, claimPath(dir, PANE_KEY));
  assert.deepEqual(JSON.parse(readFileSync(written.path, "utf8")), {
    ...claim,
    updated_at: "2026-09-11T04:05:06.789Z",
  });

  // Clearing is a removal, not an empty file: the board reads "no file" as
  // "no claim" and an empty one would be a parse failure it has to tolerate.
  const cleared = publishPaneClaim({ workspace: dir, claim: null, paneKey: PANE_KEY });
  assert.equal(cleared.written, false);
  assert.equal(existsSync(cleared.path), false);
  publishPaneClaim({ workspace: dir, claim: null, paneKey: PANE_KEY }); // already gone: still quiet
});

test("the claim carries the moment it was published", () => {
  const dir = workspace();
  const before = Date.now();
  const { path } = publishPaneClaim({ workspace: dir, claim: paneClaim(PANE_ENV) });
  const after = Date.now();

  const { updated_at: stamp } = JSON.parse(readFileSync(path, "utf8"));
  assert.match(stamp, /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/, "RFC 3339, ms, Z");
  const at = Date.parse(stamp);
  assert.ok(before <= at && at <= after, `${stamp} is the moment of the write`);
});

test("two panes in one workspace publish side by side, and clear apart", () => {
  const dir = workspace();
  const first = paneClaim(PANE_ENV, { role: "planner", taskId: "task-alpha" });
  const second = paneClaim({ ...PANE_ENV, ORCA_PANE_KEY: OTHER_KEY }, { role: "builder" });
  publishPaneClaim({ workspace: dir, claim: first });
  publishPaneClaim({ workspace: dir, claim: second });

  // The whole point of the directory: the second mount does not evict the first,
  // which is what a single workspace-wide file did to a running sibling pane.
  assert.equal(JSON.parse(readFileSync(claimPath(dir, PANE_KEY), "utf8")).pane_key, PANE_KEY);
  assert.equal(JSON.parse(readFileSync(claimPath(dir, OTHER_KEY), "utf8")).pane_key, OTHER_KEY);
  assert.deepEqual(readdirSync(join(dir, PANE_CLAIMS_RELATIVE_DIR)).sort(), [
    paneClaimFileName(OTHER_KEY),
    paneClaimFileName(PANE_KEY),
  ].sort());

  // And clearing one leaves the other: a pane may only ever retire its own file.
  publishPaneClaim({ workspace: dir, claim: null, paneKey: OTHER_KEY });
  assert.equal(existsSync(claimPath(dir, OTHER_KEY)), false);
  assert.equal(JSON.parse(readFileSync(claimPath(dir, PANE_KEY), "utf8")).role, "planner");
});

test("a claim replaces this pane's older one instead of merging with it", () => {
  const dir = workspace();
  const path = publishPaneClaim({ workspace: dir, claim: paneClaim(PANE_ENV, { role: "planner" }) }).path;
  publishPaneClaim({
    workspace: dir,
    claim: paneClaim({ ...PANE_ENV, ORCA_PANE_KEY: PANE_KEY }, { role: "builder", taskId: "task-beta" }),
  });

  const reread = JSON.parse(readFileSync(path, "utf8"));
  assert.equal(reread.role, "builder");
  assert.equal(reread.task_id, "task-beta");
  assert.equal(readdirSync(join(dir, PANE_CLAIMS_RELATIVE_DIR)).length, 1, "still this pane's one file");
});

test("no workspace, no pane or an unwritable one is a quiet no-op", () => {
  assert.deepEqual(publishPaneClaim({ workspace: "", claim: { pane_key: PANE_KEY } }), {
    written: false,
    path: null,
  });
  // A clear names its pane whatever it came from: without one there is no file
  // to remove, and removing nothing is not a failure.
  assert.deepEqual(publishPaneClaim({ workspace: "/tmp", claim: null }), { written: false, path: null });
  assert.deepEqual(publishPaneClaim({ workspace: "/tmp", claim: null, paneKey: "../escape:leaf" }), {
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
  mkdirSync(join(dir, PANE_CLAIMS_RELATIVE_DIR), { recursive: true });
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

  const target = claimPath(dir, PANE_KEY);
  assert.equal(renames.length, 1);
  assert.equal(renames[0].from, `${target}.${process.pid}.tmp`);
  assert.equal(renames[0].to, target);
});
