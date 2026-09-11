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
// One file per pane: `<workspace>/.onlyne/cache/pi-panes/<pane_key>.json`, the
// pane key with `:` flattened to `-`. The client admits one client per
// workspace (`onlyne: client already running with pid {pid}`,
// crates/onlyne-client/src/daemon.rs), but one role slot of it runs up to
// `max_sessions` sessions at once, each in its own Orca pane
// (crates/onlyne-client/src/dispatch.rs; the Orca backend issues one
// `orca terminal create` per session) — so N pi processes publish into the same
// workspace. One file for all of them would be last-writer-wins: the pane that
// mounted last would erase the panes still running, and the board would hide
// them as somebody else's tabs. A pane therefore writes only its own file and
// clears only its own file, and two panes never touch each other's claim.
//
// A claim left behind by a pane that died without clearing is harmless: the
// board intersects its tab list with the claimed panes, so a dead pane's claim
// matches no row.
//
// `role` and `task_id` are informational — the board joins on `pane_key` —
// while `updated_at` (RFC 3339 with milliseconds, Z) is what orders two claims
// that name the same pane.

import { mkdirSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

/** Workspace-relative directory of the claims; `.onlyne/cache` is ours already. */
export const PANE_CLAIMS_RELATIVE_DIR = join(".onlyne", "cache", "pi-panes");

/**
 * The file one pane's claim lives in, or null for a key that would name
 * something other than a plain file in that directory. The key is inherited
 * from the environment, so it is never trusted as a path segment: only the
 * flattened spelling of a pane key is a file name here.
 *
 * @param {string | null | undefined} paneKey
 */
export function paneClaimFileName(paneKey) {
  const name = String(paneKey ?? "").replace(/:/g, "-");
  return /^[A-Za-z0-9._-]+$/.test(name) ? `${name}.json` : null;
}

/**
 * The pane key an environment names, or null when it names no pane (a plain
 * shell, a CI run, an Orca build that exports nothing).
 *
 * @param {Record<string, string | undefined>} env
 */
export function paneKeyFrom(env) {
  const paneKey = typeof env?.ORCA_PANE_KEY === "string" ? env.ORCA_PANE_KEY : "";
  return paneKey.includes(":") ? paneKey : null;
}

/**
 * The claim for this process, or null outside an Orca pane. A claim names the
 * pane only: a missing `role` or `task_id` never makes it invalid, because the
 * pane is what the board attributes the tab by.
 *
 * @param {Record<string, string | undefined>} env
 * @param {{ role?: string | null, taskId?: string | null } | null} identity
 */
export function paneClaim(env, identity = null) {
  const paneKey = paneKeyFrom(env);
  if (!paneKey) return null;
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
 * Publish, or with a null claim drop, the claim of one pane in one workspace.
 * The pane is `claim.pane_key`, or `paneKey` when there is no claim to read it
 * from — a clear carries the key it is clearing.
 *
 * Only this pane's own file is touched. The pre-v1 single file,
 * `<workspace>/.onlyne/cache/pi-pane.json`, is never written here: in one
 * workspace it belonged to all the panes at once, so a pane that cleared it
 * would erase a live sibling's claim. The board still reads it for as long as
 * an older publisher may be running (integrations/orca-plugin/src/claims.mjs).
 *
 * A claim is written through a temporary file and renamed, so a reader never
 * sees a half-written one. Nothing here throws: a claim is a courtesy to the
 * board, and an unwritable cache directory must not fail a session.
 *
 * @param {{ workspace?: string, claim?: object | null, paneKey?: string | null, fs?: object, now?: () => string }} options
 * @returns {{ written: boolean, path: string | null }}
 */
export function publishPaneClaim({
  workspace,
  claim,
  paneKey,
  fs,
  now = () => new Date().toISOString(),
} = {}) {
  const key = claim?.pane_key ?? paneKey ?? null;
  const name = paneClaimFileName(key);
  if (!workspace || !name) return { written: false, path: null };
  const io = fs ?? { mkdirSync, writeFileSync, renameSync, rmSync };
  const target = join(workspace, PANE_CLAIMS_RELATIVE_DIR, name);
  try {
    if (!claim) {
      // Gone is the state a clear asks for, so clearing twice is a success.
      io.rmSync(target, { force: true });
      return { written: false, path: target };
    }
    io.mkdirSync(dirname(target), { recursive: true });
    const temp = `${target}.${process.pid}.tmp`;
    const stamp = { ...claim, updated_at: now() };
    io.writeFileSync(temp, `${JSON.stringify(stamp, null, 2)}\n`, "utf8");
    io.renameSync(temp, target);
    return { written: true, path: target };
  } catch {
    return { written: false, path: target };
  }
}
