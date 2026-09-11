// The pi adapter's pane claims are the board's authority on swarm membership
// (see src/claims.mjs). These tests pin what the board does with them: prefer a
// claim when one exists, fall back to the worktree when none does, take every
// pane of a workspace rather than the last one to publish, and never let a
// broken file turn into an error.

import assert from "node:assert/strict";
import { join } from "node:path";
import { test } from "node:test";

import { PANE_CLAIM_RELATIVE_PATH, PANE_CLAIMS_RELATIVE_DIR, collectBoard, scopeByClaims } from "./board.mjs";
import { createClaimReader } from "./claims.mjs";
import { fakeOnlyne, fakeOrca, roleRow, sessionRow, tabRow } from "./testing.mjs";

// The claim lives in the pi workspace; the board's session axis is configured
// by server root. Two different directories, which is the whole point.
const ROOT = "/srv/cluster";
const WS = "/srv/swarm/planner";
const OTHER_WS = "/srv/swarm/builder";
const PANE = "45e603f7-0772-48aa-bcf6-832272747713:b6d067b6-9255-4f5c-a13f-24f194ea0560";

/**
 * An in-memory stand-in for the claim files of the workspaces under test: a map
 * of absolute path to file text, with an optional mtime. `statFile` always
 * answers, so a test that cares about the mtime fallback states one.
 */
function fakeDisk(files) {
  const entries = Object.fromEntries(
    Object.entries(files).map(([path, value]) => [
      path,
      typeof value === "string" ? { text: value, mtimeMs: 0 } : { mtimeMs: 0, ...value },
    ])
  );
  return {
    readFile: (path) => {
      if (!(path in entries)) throw Object.assign(new Error(`ENOENT: ${path}`), { code: "ENOENT" });
      return entries[path].text;
    },
    statFile: (path) => {
      if (!(path in entries)) throw new Error(`ENOENT: ${path}`);
      return { mtimeMs: entries[path].mtimeMs };
    },
    readdir: (dir) => {
      const prefix = `${dir}/`;
      const names = Object.keys(entries)
        .filter((path) => path.startsWith(prefix))
        .map((path) => path.slice(prefix.length))
        .filter((name) => name && !name.includes("/"));
      if (names.length === 0) throw new Error(`ENOENT: ${dir}`);
      return names;
    },
  };
}

/** One claim file of one pane; the name carries no meaning to the reader. */
function paneFile(name, overrides = {}) {
  return [join(WS, PANE_CLAIMS_RELATIVE_DIR, `${name}.json`), claimText(overrides)];
}

function claimText(overrides = {}) {
  return JSON.stringify({
    pane_key: PANE,
    tab_id: "45e603f7-0772-48aa-bcf6-832272747713",
    leaf_id: "b6d067b6-9255-4f5c-a13f-24f194ea0560",
    handle: "term_11111111-1111-4111-8111-111111111111",
    worktree_id: "2ea2fe23-829c-4a8f-bcac-4129eb78a164",
    role: "planner",
    task_id: "task-alpha",
    updated_at: "2026-09-11T04:00:00.000Z",
    ...overrides,
  });
}

function readerFor(files) {
  return createClaimReader(fakeDisk(files));
}

test("a claim reader turns a pane's own file into a pane claim", () => {
  const { claims, unpublished } = readerFor(Object.fromEntries([paneFile("pane-alpha")]))([WS]);

  assert.equal(claims.length, 1);
  assert.equal(claims[0].paneKey, PANE);
  assert.equal(claims[0].role, "planner");
  assert.equal(claims[0].workspace, WS);
  assert.equal(claims[0].updatedAt, Date.parse("2026-09-11T04:00:00.000Z"));
  assert.deepEqual(unpublished, []);
});

test("every pane of a workspace is a claim, not just the last to publish", () => {
  // The single-file layout this replaced kept one claim per workspace, so a pane
  // mounting after a running one evicted it and the board hid a live tab.
  const other = "45e603f7-0772-48aa-bcf6-832272747713:aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
  const { claims, unpublished } = readerFor(
    Object.fromEntries([
      paneFile("pane-alpha"),
      paneFile("pane-beta", { pane_key: other, leaf_id: "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee", role: "builder" }),
    ])
  )([WS]);

  assert.deepEqual(
    claims.map((claim) => claim.paneKey).sort(),
    [PANE, other].sort(),
    "both panes are claimed"
  );
  assert.equal(claims.find((claim) => claim.paneKey === other).role, "builder");
  assert.deepEqual(unpublished, []);
});

test("a workspace that publishes nothing is reported, not silently dropped", () => {
  // No directory and no legacy file: nothing to read.
  assert.deepEqual(readerFor({})([WS]), { claims: [], unpublished: [WS] });

  // Readable but unusable counts as nothing published: the file that is there
  // is not a claim, so naming the workspace is the honest answer either way.
  const malformed = readerFor(
    Object.fromEntries([
      [join(WS, PANE_CLAIMS_RELATIVE_DIR, "pane-alpha.json"), "{ not json"],
      [join(OTHER_WS, PANE_CLAIM_RELATIVE_PATH), "{ not json"],
    ])
  );
  assert.deepEqual(malformed([WS, OTHER_WS]), { claims: [], unpublished: [WS, OTHER_WS] });

  const noPane = readerFor(
    Object.fromEntries([[join(WS, PANE_CLAIMS_RELATIVE_DIR, "pane-alpha.json"), JSON.stringify({ role: "planner" })]])
  );
  assert.deepEqual(noPane([WS]), { claims: [], unpublished: [WS] }, "a claim without a pane key attributes nothing");

  assert.deepEqual(readerFor({})([]), { claims: [], unpublished: [] });
});

test("a broken file costs its own claim and nothing else", () => {
  const { claims, unpublished } = readerFor(
    Object.fromEntries([
      paneFile("pane-alpha"),
      [join(WS, PANE_CLAIMS_RELATIVE_DIR, "pane-beta.json"), "{ not json"],
      [join(WS, PANE_CLAIMS_RELATIVE_DIR, "pane-gamma.json"), JSON.stringify({ role: "builder" })],
      // The publisher's temporary file: not a claim, and not an error either.
      [join(WS, PANE_CLAIMS_RELATIVE_DIR, "pane-delta.json.1234.tmp"), claimText()],
    ])
  )([WS]);

  assert.deepEqual(claims.map((claim) => claim.paneKey), [PANE], "the one usable file is the one claim");
  assert.deepEqual(unpublished, [], "a workspace that published is published");
});

test("the pre-v1 workspace file is still read", () => {
  const legacy = Object.fromEntries([[join(WS, PANE_CLAIM_RELATIVE_PATH), claimText()]]);
  const { claims, unpublished } = readerFor(legacy)([WS]);

  assert.equal(claims.length, 1, "a pi process that predates the directory still attributes its pane");
  assert.equal(claims[0].paneKey, PANE);
  assert.equal(claims[0].workspace, WS);
  assert.deepEqual(unpublished, []);

  // And it is the same pane, not a second one: the two files fold together.
  const both = readerFor({ ...legacy, ...Object.fromEntries([paneFile("pane-alpha")]) })([WS]);
  assert.equal(both.claims.length, 1, "one pane key is one claim");
});

test("for one pane the newer claim wins, whichever file it came from", () => {
  const legacyPath = join(WS, PANE_CLAIM_RELATIVE_PATH);
  const stale = claimText({ role: "stale", updated_at: "2026-09-11T04:00:00.000Z" });
  const fresh = claimText({ role: "fresh", updated_at: "2026-09-11T04:00:05.000Z" });

  const directoryWins = readerFor({ ...Object.fromEntries([paneFile("pane-alpha", { role: "fresh", updated_at: "2026-09-11T04:00:05.000Z" })]), [legacyPath]: stale });
  assert.equal(directoryWins([WS]).claims[0].role, "fresh");

  const legacyWins = readerFor({ ...Object.fromEntries([paneFile("pane-alpha", { role: "stale", updated_at: "2026-09-11T04:00:00.000Z" })]), [legacyPath]: fresh });
  assert.equal(legacyWins([WS]).claims[0].role, "fresh", "a legacy file that is newer still wins");

  // A file written before the stamp existed carries no time of its own, so its
  // own mtime is what it is compared by — here newer than the stamped sibling.
  const byMtime = readerFor({
    ...Object.fromEntries([paneFile("pane-alpha", { role: "stamped", updated_at: "2026-09-11T04:00:00.000Z" })]),
    [legacyPath]: { text: claimText({ role: "undated", updated_at: undefined }), mtimeMs: Date.parse("2026-09-11T04:00:09.000Z") },
  });
  assert.equal(byMtime([WS]).claims[0].role, "undated");

  // With nothing to compare, the first claim read stands rather than flapping.
  const undated = readerFor({
    ...Object.fromEntries([paneFile("pane-alpha", { role: "first", updated_at: undefined })]),
    [legacyPath]: { text: claimText({ role: "second", updated_at: undefined }), mtimeMs: 0 },
  });
  assert.equal(undated([WS]).claims[0].role, "first");
});

test("workspaces that publish the same pane are one claim, none unpublished", () => {
  const same = Object.fromEntries([paneFile("pane-alpha")]);
  const elsewhere = { [join(OTHER_WS, PANE_CLAIMS_RELATIVE_DIR, "pane-alpha.json")]: claimText() };
  const { claims, unpublished } = readerFor({ ...same, ...elsewhere })([WS, OTHER_WS]);

  assert.equal(claims.length, 1, "one pane key is one claim");
  assert.equal(claims[0].workspace, WS, "the claim keeps the workspace it was read from");
  assert.deepEqual(unpublished, [], "both workspaces published, even if identically");
});

test("a claim wins over the worktree heuristic", async () => {
  const board = await collectBoard({
    orca: fakeOrca({
      tabs: [
        // The adapter's pane, in the swarm's worktree.
        tabRow({ worktreePath: "/repo/swarm" }),
        // Another pi in the *same* worktree: the heuristic would keep it, the
        // claim knows better.
        tabRow({
          handle: "term_foreign",
          tabId: "aaaa1111-1111-4111-8111-111111111111",
          leafId: "bbbb2222-2222-4222-8222-222222222222",
          title: "π - somebody else's task",
          worktreePath: "/repo/swarm",
        }),
      ],
    }),
    onlyne: fakeOnlyne({ sessions: { [ROOT]: [sessionRow()] }, roles: { [ROOT]: [roleRow()] } }),
    serverRoots: [ROOT],
    readClaims: () => ({ claims: [{ paneKey: PANE, workspace: WS }], unpublished: [] }),
  });

  assert.equal(board.scope.source, "adapter");
  assert.deepEqual(board.tabs.map((tab) => tab.handle), ["term_11111111-1111-4111-8111-111111111111"]);
  assert.equal(board.summary.hiddenTabs, 1, "the unclaimed pi in the same worktree is hidden");
  assert.equal(board.rows[0].joined, true);
});

test("a stale claim for a pane Orca no longer lists hides nothing else", async () => {
  const board = await collectBoard({
    orca: fakeOrca({ tabs: [tabRow({ worktreePath: "/repo/swarm" })] }),
    onlyne: fakeOnlyne({ sessions: { [ROOT]: [sessionRow()] }, roles: { [ROOT]: [roleRow()] } }),
    serverRoots: [ROOT],
    readClaims: () => ({ claims: [{ paneKey: "gone-tab:gone-leaf", workspace: WS }], unpublished: [] }),
  });

  // Claims exist, so the heuristic is off; nothing matches, so the tab axis is
  // empty and the board says so rather than silently reverting to "everything".
  assert.equal(board.scope.source, "adapter");
  assert.equal(board.summary.tabs, 0);
  assert.equal(board.scope.claimed, 1);
});

test("no claim falls back to the worktree heuristic", async () => {
  const board = await collectBoard({
    orca: fakeOrca({ tabs: [tabRow({ worktreePath: "/repo/swarm" }), tabRow({ handle: "term_x" })] }),
    onlyne: fakeOnlyne({ sessions: { [ROOT]: [sessionRow()] }, roles: { [ROOT]: [roleRow()] } }),
    serverRoots: ["/repo/swarm/cluster"],
    readClaims: () => ({ claims: [], unpublished: [] }),
  });

  assert.equal(board.scope.source, "worktree");
  assert.equal(board.summary.tabs, 1);
  assert.equal(board.scope.hidden, 1);
});

test("scopeByClaims reports the worktrees it kept and the claims it saw", () => {
  // Raw fixtures carry no derived `paneKey`, so state the keys explicitly: the
  // board itself always sees normalized rows (src/orca-cli.mjs).
  const tabs = [
    tabRow({ paneKey: PANE, worktreePath: "/repo/swarm" }),
    tabRow({ handle: "term_other", paneKey: "t:l", worktreePath: "/repo/x" }),
  ];
  const scoped = scopeByClaims(tabs, [{ paneKey: PANE }, { paneKey: "t:l" }]);

  assert.deepEqual(scoped.scope.worktrees, ["/repo/swarm", "/repo/x"]);
  assert.equal(scoped.scope.claimed, 2);
  assert.equal(scoped.scope.hidden, 0);
  assert.equal(scopeByClaims(tabs, []), null, "no claims: the caller decides");
});
