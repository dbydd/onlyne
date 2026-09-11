// Reading the pi adapter's pane claims off disk.
//
// The adapter is the only component that knows, from inside the process, which
// Orca pane is an onlyne session: it inherits `ORCA_PANE_KEY` (and the tab,
// handle and worktree ids) from the tab it was spawned in. It publishes that
// binding as one file per pane —
// `<workspace>/.onlyne/cache/pi-panes/<pane_key with ':' flattened to '-'>.json`
// — and this module turns the claims of the configured pi workspaces into the
// board's authority.
//
// The directory is the point. One workspace runs N pi processes — one client
// per workspace (`onlyne: client already running with pid {pid}`,
// crates/onlyne-client/src/daemon.rs), but up to `max_sessions` sessions per
// role slot, each spawned into its own Orca pane
// (crates/onlyne-client/src/dispatch.rs) — so a single claim file for a
// workspace would be last-writer-wins, and the board would hide every pane but
// the one that mounted last. The reader therefore takes the union of the files
// in that directory.
//
// The pre-v1 single file `<workspace>/.onlyne/cache/pi-pane.json` is folded in
// as well: a pi process that was already running when this reader was upgraded
// keeps writing it while its pane is live, and ignoring it would hide that
// pane. It is a transition read, not a second authority — a file loses to a
// fresher claim for the same pane key (`updated_at`, falling back to the file's
// own mtime), and nothing writes it any more.
//
// A broken file is one claim fewer, never an error: a claim is a courtesy, and
// nothing about a workspace's tabs should depend on a half-written cache file.
//
// The reader is given the workspaces to look in (`piWorkspaces` in config),
// never the server roots: the two are different directories, and the client —
// not the server root — owns the workspace path.
//
// A workspace that contributed no claim is not an error — a swarm boots pi after
// the board — but it is not silent either: `piWorkspaces` is a hand-written list,
// and a typo in it would otherwise look exactly like a swarm that has not mounted
// yet. The reader therefore reports those workspaces back, and the panel names
// them (src/panel-document.mjs).

import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import {
  PANE_CLAIM_RELATIVE_PATH,
  PANE_CLAIMS_RELATIVE_DIR,
  claimRecency,
  readPaneClaim,
} from "./board.mjs";

/**
 * One claim file, or null for a file that is absent, unreadable or unusable.
 * The mtime is read separately because it is only ever a fallback: a file that
 * cannot be stat'ed still yields the claim its own contents carry.
 */
function readClaimFile(path, { readFile, statFile }) {
  let text;
  try {
    text = readFile(path, "utf8");
  } catch {
    return null;
  }
  let mtime = null;
  try {
    const stats = statFile(path);
    if (typeof stats?.mtimeMs === "number") mtime = stats.mtimeMs;
  } catch {
    mtime = null;
  }
  return readPaneClaim(text, { mtime });
}

/**
 * Every claim one workspace holds: the directory of per-pane files, in name
 * order, then the legacy single file. A directory that is missing or is not a
 * directory contributes nothing.
 */
function readWorkspaceClaims(workspace, io) {
  const claims = [];
  const dir = join(workspace, PANE_CLAIMS_RELATIVE_DIR);
  let names = [];
  try {
    names = io.readdir(dir);
  } catch {
    names = [];
  }
  for (const name of [...names].sort()) {
    // The publisher writes through `<name>.tmp`, which never ends in `.json`.
    if (!name.endsWith(".json")) continue;
    const claim = readClaimFile(join(dir, name), io);
    if (claim) claims.push(claim);
  }
  const legacy = readClaimFile(join(workspace, PANE_CLAIM_RELATIVE_PATH), io);
  if (legacy) claims.push(legacy);
  return claims;
}

export function createClaimReader({
  readFile = readFileSync,
  statFile = statSync,
  readdir = readdirSync,
} = {}) {
  /**
   * @param {string[]} workspaces configured pi workspaces, in config order
   * @returns {{ claims: Array<object>, unpublished: string[] }} the union of the
   *   workspaces' claims, deduplicated by pane key with the newest claim for a
   *   key winning, and the workspaces that produced none — because no claim file
   *   was readable, or none that parsed carried a pane key
   */
  return function readClaims(workspaces) {
    const io = { readFile, statFile, readdir };
    // Insertion order is the workspaces' own order and then the file names': a
    // board that reshuffles rows for no reason is a board nobody can read twice.
    const byPane = new Map();
    const unpublished = [];
    for (const workspace of workspaces ?? []) {
      const found = readWorkspaceClaims(workspace, io);
      if (!found.length) {
        unpublished.push(workspace);
        continue;
      }
      for (const claim of found) {
        const held = byPane.get(claim.paneKey);
        if (!held || claimRecency(claim) > claimRecency(held)) {
          byPane.set(claim.paneKey, { ...claim, workspace });
        }
      }
    }
    return { claims: [...byPane.values()], unpublished };
  };
}
