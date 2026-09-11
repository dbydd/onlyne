// The Orca pane this pi process lives in, published for the supervisor board.
//
// `integrations/orca-plugin` filters its tab axis to one swarm's panes, and the
// authority for that filter is this file. An Orca pane exports `ORCA_PANE_KEY`
// (beside `ORCA_TAB_ID`, `ORCA_TERMINAL_HANDLE` and `ORCA_WORKTREE_ID`) into the
// command it was started with — measured 2026-09-11 on Orca 1.4.198 — and the
// client spawns this plugin with the environment it inherited, so the binding is
// inherited rather than guessed: this process is the only component that can
// state, from the inside, which Orca pane is an onlyne session.
//
// One file per workspace, `<workspace>/.onlyne/cache/pi-pane.json`. A workspace
// runs at most one client (`onlyne: client already running with pid {pid}`,
// crates/onlyne-client/src/daemon.rs), so the file cannot be ambiguous between
// two panes, and the board reads it from the workspaces listed in its own
// `piWorkspaces` config. `role` and `task_id` are informational: the board joins
// on `pane_key`.

import { mkdirSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

/** Workspace-relative location of the claim; `.onlyne/cache` is ours already. */
export const PANE_CLAIM_RELATIVE_PATH = join(".onlyne", "cache", "pi-pane.json");

/**
 * The claim for this process, or null outside an Orca pane (a plain shell, a CI
 * run, an Orca build that exports nothing). A claim names the pane only: a
 * missing `role` or `task_id` never makes it invalid, because the pane is what
 * the board attributes the tab by.
 *
 * @param {Record<string, string | undefined>} env
 * @param {{ role?: string | null, taskId?: string | null } | null} identity
 */
export function paneClaim(env, identity = null) {
  const paneKey = typeof env?.ORCA_PANE_KEY === "string" ? env.ORCA_PANE_KEY : "";
  if (!paneKey.includes(":")) return null;
  const [tabId, leafId] = paneKey.split(":");
  return {
    pane_key: paneKey,
    tab_id: env.ORCA_TAB_ID || tabId,
    leaf_id: leafId,
    handle: env.ORCA_TERMINAL_HANDLE ?? null,
    worktree_id: env.ORCA_WORKTREE_ID ?? null,
    role: identity?.role ?? null,
    task_id: identity?.taskId ?? null,
  };
}

/**
 * Publish (or, with a null claim, drop) the claim for one workspace.
 *
 * A claim is written through a temporary file and renamed, so a reader never
 * sees a half-written one. Nothing here throws: a claim is a courtesy to the
 * board, and an unwritable cache directory must not fail a session.
 *
 * @param {{ workspace?: string, claim?: object | null, fs?: object }} options
 * @returns {{ written: boolean, path: string | null }}
 */
export function publishPaneClaim({ workspace, claim, fs } = {}) {
  if (!workspace) return { written: false, path: null };
  const io = fs ?? { mkdirSync, writeFileSync, renameSync, rmSync };
  const target = join(workspace, PANE_CLAIM_RELATIVE_PATH);
  try {
    if (!claim) {
      io.rmSync(target, { force: true });
      return { written: false, path: target };
    }
    io.mkdirSync(dirname(target), { recursive: true });
    const temp = `${target}.${process.pid}.tmp`;
    io.writeFileSync(temp, `${JSON.stringify(claim, null, 2)}\n`, "utf8");
    io.renameSync(temp, target);
    return { written: true, path: target };
  } catch {
    return { written: false, path: target };
  }
}
