// Reading the pi adapter's pane claims off disk.
//
// The adapter is the only component that knows, from inside the process, which
// Orca pane is an onlyne session: it inherits `ORCA_PANE_KEY` (and the tab,
// handle and worktree ids) from the tab it was spawned in. It publishes that
// binding as `<role workspace>/.onlyne/cache/pi-pane.json`, and this module
// turns the claims of the configured pi workspaces into the board's authority.
//
// One claim file per workspace, and the client enforces one client per
// workspace (`onlyne: client already running with pid {pid}`,
// crates/onlyne-client/src/daemon.rs), so a claim can never be ambiguous
// between two panes: it is the workspace's own client that wrote it.
//
// The reader is given the workspaces to look in (`piWorkspaces` in config),
// never the server roots: the two are different directories, and the client —
// not the server root — owns the workspace path. A missing, unreadable or
// malformed file is a normal state, never an error: it only means that
// workspace has published nothing.

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { PANE_CLAIM_RELATIVE_PATH, readPaneClaim } from "./board.mjs";

export function createClaimReader({ readFile = readFileSync } = {}) {
  /**
   * @param {string[]} workspaces configured pi workspaces, in config order
   * @returns {Array<object>} the claims that parsed, deduplicated by pane key
   */
  return function readClaims(workspaces) {
    const claims = [];
    const seen = new Set();
    for (const workspace of workspaces ?? []) {
      let text;
      try {
        text = readFile(join(String(workspace), PANE_CLAIM_RELATIVE_PATH), "utf8");
      } catch {
        continue;
      }
      const claim = readPaneClaim(text);
      if (!claim || seen.has(claim.paneKey)) continue;
      seen.add(claim.paneKey);
      claims.push({ ...claim, workspace });
    }
    return claims;
  };
}
