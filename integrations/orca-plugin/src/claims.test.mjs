// The pi adapter's pane claims are the board's authority on swarm membership
// (see src/claims.mjs). These tests pin what the board does with them: prefer a
// claim when one exists, fall back to the worktree when none does, and never
// let a broken file turn into an error.

import assert from "node:assert/strict";
import { test } from "node:test";

import { collectBoard, scopeByClaims } from "./board.mjs";
import { createClaimReader } from "./claims.mjs";
import { fakeOnlyne, fakeOrca, roleRow, sessionRow, tabRow } from "./testing.mjs";

// The claim lives in the pi workspace; the board's session axis is configured
// by server root. Two different directories, which is the whole point.
const ROOT = "/srv/cluster";
const WS = "/srv/swarm/planner";
const PANE = "45e603f7-0772-48aa-bcf6-832272747713:b6d067b6-9255-4f5c-a13f-24f194ea0560";

function claimFile(overrides = {}) {
  return JSON.stringify({
    pane_key: PANE,
    tab_id: "45e603f7-0772-48aa-bcf6-832272747713",
    leaf_id: "b6d067b6-9255-4f5c-a13f-24f194ea0560",
    handle: "term_11111111-1111-4111-8111-111111111111",
    worktree_id: "2ea2fe23-829c-4a8f-bcac-4129eb78a164",
    role: "planner",
    task_id: "task-alpha",
    ...overrides,
  });
}

test("a claim reader turns the adapter's file into a pane claim", () => {
  const reader = createClaimReader({ readFile: () => claimFile() });
  const claims = reader([WS]);

  assert.equal(claims.length, 1);
  assert.equal(claims[0].paneKey, PANE);
  assert.equal(claims[0].role, "planner");
  assert.equal(claims[0].workspace, WS);
});

test("a missing, malformed or duplicated claim degrades to fewer claims", () => {
  const missing = createClaimReader({
    readFile: () => {
      throw new Error("ENOENT");
    },
  });
  assert.deepEqual(missing([WS]), []);

  const malformed = createClaimReader({ readFile: () => "{ not json" });
  assert.deepEqual(malformed([WS]), []);

  const noPane = createClaimReader({ readFile: () => JSON.stringify({ role: "planner" }) });
  assert.deepEqual(noPane([WS]), [], "a claim without a pane key attributes nothing");

  // Two roots publishing the same pane key is one claim, not two.
  const duplicated = createClaimReader({ readFile: () => claimFile() });
  assert.equal(duplicated([WS, "/srv/swarm/builder"]).length, 1);
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
    readClaims: () => [{ paneKey: PANE, workspace: WS }],
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
    readClaims: () => [{ paneKey: "gone-tab:gone-leaf", workspace: WS }],
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
    readClaims: () => [],
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
