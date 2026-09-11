// The supervisor board: two independent axes, joined only where a title says so.
//
// Axis A — Orca tabs. One flat `orca terminal list --json` call, then scoped to
// the swarm: in the Orca scenario one worktree holds one onlyne server, so a tab
// belongs to this board only when its `worktreePath` contains one of the
// configured server roots. Everything else in Orca — other repositories, other
// agents, an unrelated pi in another worktree — is somebody else's tab and is
// counted, not listed.
//
// Axis B — onlyne admin. For each configured server root, `sessions` supplies
// the task rows and `roles` the role skeleton. Authority for session identity
// lives in the adapter / pi plugin protocol; this board only mirrors what that
// admin surface reports.
//
// The join is WEAK and display-only. A tab whose trimmed title equals
// `onlyne:<task_id>` is annotated onto that task's row. OSC title writes can
// steal that prefix — any process in the pane may set the title — so the
// annotation is a convenience for the supervisor, never evidence: a missing or
// wrong annotation says nothing about the session. A tab matching no task stays
// a standalone tab row; a task with no tab stays an unjoined task row.
//
// Every absence degrades instead of failing: a failed tab list, an unreachable
// root, a CLI that does not know the verb, and a scope that cannot be derived
// (a server root outside every Orca worktree keeps the unscoped list rather than
// rendering an empty board). Nothing here throws and nothing here writes.

import { realpathSync } from "node:fs";
import { resolve, sep } from "node:path";

export const UNKNOWN_ROLE = "(unknown role)";
export const TITLE_PREFIX = "onlyne:";

/** Best-effort canonical path: Orca reports canonical worktree paths. */
function canonical(path, realpath) {
  try {
    return realpath(path);
  } catch {
    return resolve(path);
  }
}

/** Boundary-aware containment: `/a/b` is inside `/a`, never inside `/ab`. */
function isInside(parent, child) {
  if (parent === child) return true;
  const prefix = parent.endsWith(sep) ? parent : `${parent}${sep}`;
  return child.startsWith(prefix);
}

/**
 * Restrict the tab axis to the swarm.
 *
 * One worktree per server is the Orca assumption this rests on: a session tab
 * lands in the worktree its supervisor's tab runs in, so the tabs of a swarm
 * are exactly the tabs of the worktrees its server roots live in. A tab whose
 * worktree contains no configured server root is another swarm's (or another
 * tool's) tab and is hidden — listed only in the hidden count.
 *
 * When no tab carries any configured root's worktree, the scope cannot be
 * derived (the server root may live outside Orca entirely, as in the e2e
 * harness), and hiding everything would read as a broken board: the unscoped
 * list is kept and `derived` says so.
 *
 * @returns {{tabs: Array, scope: {derived: boolean, worktrees: string[], hidden: number}}}
 */
export function scopeTabs(tabs, serverRoots, { realpath = realpathSync } = {}) {
  const roots = (serverRoots ?? []).map((root) => canonical(String(root), realpath));
  if (!roots.length) {
    return { tabs, scope: { derived: false, source: "none", worktrees: [], hidden: 0 } };
  }
  const kept = [];
  const worktrees = new Set();
  for (const tab of tabs) {
    const path = typeof tab.worktreePath === "string" && tab.worktreePath
      ? canonical(tab.worktreePath, realpath)
      : null;
    const owner = path ? roots.find((root) => isInside(path, root)) : undefined;
    if (!owner) continue;
    worktrees.add(path);
    kept.push(tab);
  }
  if (!kept.length) {
    return { tabs, scope: { derived: false, source: "none", worktrees: [], hidden: 0 } };
  }
  return {
    tabs: kept,
    scope: {
      derived: true,
      source: "worktree",
      worktrees: [...worktrees].sort(),
      hidden: tabs.length - kept.length,
    },
  };
}

/**
 * Where the pi adapter publishes the pane it mounted in, inside its own
 * workspace: `<workspace>/.onlyne/cache/pi-pane.json`. The pi plugin inherits
 * `ORCA_PANE_KEY` / `ORCA_TAB_ID` / `ORCA_TERMINAL_HANDLE` / `ORCA_WORKTREE_ID`
 * from the Orca tab it was spawned in (measured 2026-09-11 on 1.4.198: an
 * `orca terminal create --command …` pane exports all four), so it is the one
 * component that knows — from the inside — which Orca pane is an onlyne session.
 * That is the authority; the worktree heuristic in `scopeTabs` is the fallback
 * for a swarm whose pi processes have not published anything yet.
 */
export const PANE_CLAIM_RELATIVE_PATH = ".onlyne/cache/pi-pane.json";

/** The pane identity a claim carries, or null for a malformed/closed claim. */
export function readPaneClaim(text) {
  let parsed;
  try {
    parsed = JSON.parse(text);
  } catch {
    return null;
  }
  if (!parsed || typeof parsed !== "object") return null;
  const paneKey = typeof parsed.pane_key === "string" && parsed.pane_key ? parsed.pane_key : null;
  if (!paneKey) return null;
  return {
    paneKey,
    tabId: typeof parsed.tab_id === "string" ? parsed.tab_id : null,
    leafId: typeof parsed.leaf_id === "string" ? parsed.leaf_id : null,
    handle: typeof parsed.handle === "string" ? parsed.handle : null,
    worktreeId: typeof parsed.worktree_id === "string" ? parsed.worktree_id : null,
    role: typeof parsed.role === "string" ? parsed.role : null,
    taskId: typeof parsed.task_id === "string" ? parsed.task_id : null,
  };
}

/**
 * Scope the tab axis by adapter claims when any exist, else by worktree.
 *
 * Authority order, and why: the pi adapter publishes the pane it lives in, so a
 * claimed pane is an onlyne session by construction. The worktree heuristic
 * cannot tell one swarm's pi from another process in the same worktree, so it
 * only applies while no claim has been published — a board that drops the whole
 * tab axis because a swarm is still booting would be worse than a permissive
 * one.
 */
export function scopeByClaims(tabs, claims) {
  if (!claims.length) return null;
  const claimed = new Set(claims.map((claim) => claim.paneKey));
  const kept = tabs.filter((tab) => tab.paneKey && claimed.has(tab.paneKey));
  const worktrees = [...new Set(kept.map((tab) => tab.worktreePath).filter(Boolean))].sort();
  return {
    tabs: kept,
    scope: {
      derived: true,
      source: "adapter",
      worktrees,
      hidden: tabs.length - kept.length,
      claimed: claimed.size,
    },
  };
}

/** `onlyne:<task_id>` -> `<task_id>`; any other title -> null. */
export function taskIdFromTitle(title) {
  if (typeof title !== "string") return null;
  const trimmed = title.trim();
  if (!trimmed.startsWith(TITLE_PREFIX)) return null;
  const taskId = trimmed.slice(TITLE_PREFIX.length).trim();
  return taskId || null;
}

/** task id -> tabs carrying that title, in Orca's own row order. */
export function indexTabsByTask(tabs) {
  const byTask = new Map();
  for (const tab of tabs) {
    const taskId = taskIdFromTitle(tab.title);
    if (!taskId) continue;
    const bucket = byTask.get(taskId);
    if (bucket) bucket.push(tab);
    else byTask.set(taskId, [tab]);
  }
  return byTask;
}

export function isWorking(row) {
  const lifecycle = row?.session?.lifecycle;
  const agent = row?.session?.agent;
  return lifecycle === "working" || agent === "running";
}

/** The tab-addressing fields every row mirrors, whatever its kind. */
function tabMirrors(tab) {
  return {
    tab: tab ?? null,
    handle: tab?.handle ?? null,
    paneKey: tab?.paneKey ?? null,
    tabId: tab?.tabId ?? null,
    leafId: tab?.leafId ?? null,
    title: tab?.title ?? null,
    worktreeId: tab?.worktreeId ?? null,
    worktreePath: tab?.worktreePath ?? null,
    // The one string `orca terminal list --worktree` needs for this row's tab.
    selector: tab?.worktreeId ?? null,
    connected: tab?.connected === true,
    lastOutputAt: tab?.lastOutputAt ?? null,
  };
}

function taskRow({ root, role, session, tab }) {
  return {
    kind: "task",
    root,
    role,
    taskId: session.taskId,
    sessionId: session.sessionId,
    session,
    joined: Boolean(tab),
    live: tab?.connected === true,
    ...tabMirrors(tab),
  };
}

function strayTabRow(tab) {
  return {
    kind: "tab",
    root: null,
    role: null,
    taskId: null,
    sessionId: null,
    session: null,
    joined: false,
    live: tab.connected === true,
    ...tabMirrors(tab),
  };
}

/**
 * Claim the tab for one task: the first unclaimed tab carrying that title. A
 * second tab with the same title, or the same task id on a later root, finds
 * nothing to claim and renders unjoined rather than double-counting a live tab.
 */
function claimTab(tabsByTask, claimed, taskId) {
  for (const tab of tabsByTask.get(taskId) ?? []) {
    if (claimed.has(tab.handle)) continue;
    claimed.add(tab.handle);
    return tab;
  }
  return null;
}

function scanView(result, key) {
  return result.ok
    ? { ok: true, count: result[key].length }
    : { ok: false, code: result.code, message: result.message };
}

function sortedGroups(groups) {
  const list = [...groups.values()];
  for (const group of list) {
    group.rows.sort((left, right) => left.taskId.localeCompare(right.taskId));
    group.live = group.rows.filter((row) => row.live).length;
    group.working = group.rows.filter(isWorking).length;
  }
  list.sort((left, right) => {
    if (left.role === UNKNOWN_ROLE) return right.role === UNKNOWN_ROLE ? 0 : 1;
    if (right.role === UNKNOWN_ROLE) return -1;
    return left.role.localeCompare(right.role);
  });
  return list;
}

export function summarize({ roots, tabs, allTabs = tabs, rows, strayTabs }) {
  return {
    roots: roots.length,
    rootsFailed: roots.filter((root) => root.failures.length > 0).length,
    roles: roots.reduce((total, root) => total + root.groups.length, 0),
    tabs: tabs.length,
    hiddenTabs: allTabs.length - tabs.length,
    liveTabs: tabs.filter((tab) => tab.connected === true).length,
    sessions: rows.filter((row) => row.kind === "task").length,
    sessionsWorking: rows.filter(isWorking).length,
    joined: rows.filter((row) => row.kind === "task" && row.joined).length,
    strayTabs: strayTabs.length,
    rows: rows.length,
  };
}

/**
 * @param {object} options
 * @param {object} options.orca    createOrcaCli()
 * @param {object} options.onlyne  createOnlyneCli()
 * @param {string[]} [options.serverRoots] configured roots, in config order
 * @param {Function} [options.readClaims] returns `{claims, unpublished}` — the
 *   pi adapters' pane claims and the configured workspaces that produced none
 *   (`claims.mjs`); absent means none at all, which leaves the worktree
 *   heuristic in charge of the tab axis
 * @param {Function} [options.now]
 * @returns {Promise<object>} the board; `ok` tracks the tab axis only, so an
 *   unreachable root still leaves `ok:true` with its own entry in `errors`.
 */
export async function collectBoard({
  orca,
  onlyne,
  serverRoots = [],
  readClaims,
  now = () => Date.now(),
} = {}) {
  const scannedAt = now();
  const errors = [];

  // Axis A: every tab of every worktree, flat, in Orca's own order — then cut
  // down to the panes the swarm's own adapters claim, or, with nothing claimed,
  // to the worktrees this board's server roots live in.
  const tabsScan = await orca.listTerminals();
  const allTabs = tabsScan.ok ? tabsScan.rows : [];
  if (!tabsScan.ok) {
    errors.push({ scope: "orca", axis: "tabs", code: tabsScan.code, message: tabsScan.message });
  }
  const claimRead = (readClaims ?? (() => ({ claims: [], unpublished: [] })))();
  const claims = claimRead.claims ?? [];
  const { tabs, scope } = scopeByClaims(allTabs, claims) ?? scopeTabs(allTabs, serverRoots);

  const tabsByTask = indexTabsByTask(tabs);
  const claimed = new Set();
  const roots = [];

  for (const root of serverRoots) {
    // Axis B for this root: independent calls, so one failing verb still
    // reports what the other one answered.
    const [sessionsScan, rolesScan] = await Promise.all([
      onlyne.querySessions(root),
      onlyne.queryRoles(root),
    ]);
    const failures = [];
    for (const [axis, result] of [
      ["sessions", sessionsScan],
      ["roles", rolesScan],
    ]) {
      if (result.ok) continue;
      const failure = { scope: root, axis, code: result.code, message: result.message };
      failures.push(failure);
      errors.push(failure);
    }

    const groups = new Map();
    if (rolesScan.ok) {
      for (const role of rolesScan.roles) {
        groups.set(role.role, { role: role.role, presence: role, rows: [] });
      }
    }
    let sessionsWorking = 0;
    if (sessionsScan.ok) {
      for (const session of sessionsScan.sessions) {
        const role = session.role ?? UNKNOWN_ROLE;
        let group = groups.get(role);
        if (!group) {
          group = { role, presence: null, rows: [] };
          groups.set(role, group);
        }
        const row = taskRow({ root, role, session, tab: claimTab(tabsByTask, claimed, session.taskId) });
        if (isWorking(row)) sessionsWorking += 1;
        group.rows.push(row);
      }
    }

    const sections = sortedGroups(groups);
    roots.push({
      root,
      sessionsScan: scanView(sessionsScan, "sessions"),
      rolesScan: scanView(rolesScan, "roles"),
      roles: rolesScan.ok ? rolesScan.roles : [],
      failures,
      groups: sections,
      summary: {
        roles: sections.length,
        sessions: sessionsScan.ok ? sessionsScan.sessions.length : 0,
        sessionsWorking,
      },
    });
  }

  const strayTabs = tabs.filter((tab) => !claimed.has(tab.handle)).map(strayTabRow);
  const rows = [
    ...roots.flatMap((root) => root.groups.flatMap((group) => group.rows)),
    ...strayTabs,
  ];

  return {
    claims: { published: claims.length, unpublished: claimRead.unpublished ?? [] },
    scannedAt,
    ok: tabsScan.ok,
    errors,
    tabs,
    totalTabs: allTabs.length,
    scope,
    roots,
    strayTabs,
    rows,
    summary: summarize({ roots, tabs, allTabs, rows, strayTabs }),
  };
}
