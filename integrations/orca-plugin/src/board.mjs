// The supervisor board: two independent axes, joined only where a title says so.
//
// Axis A — Orca tabs. One flat `orca terminal list --json` call. Orca is the
// supervisor's management port only: the backend no longer registers one
// worktree per role, so every session tab lands in the host worktree's list and
// a per-worktree scan has nothing to walk.
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
// root, a CLI that does not know the verb. Nothing here throws and nothing here
// writes.

export const UNKNOWN_ROLE = "(unknown role)";
export const TITLE_PREFIX = "onlyne:";

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

export function summarize({ roots, tabs, rows, strayTabs }) {
  return {
    roots: roots.length,
    rootsFailed: roots.filter((root) => root.failures.length > 0).length,
    roles: roots.reduce((total, root) => total + root.groups.length, 0),
    tabs: tabs.length,
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
 * @param {Function} [options.now]
 * @returns {Promise<object>} the board; `ok` tracks the tab axis only, so an
 *   unreachable root still leaves `ok:true` with its own entry in `errors`.
 */
export async function collectBoard({ orca, onlyne, serverRoots = [], now = () => Date.now() } = {}) {
  const scannedAt = now();
  const errors = [];

  // Axis A: every tab of every worktree, flat, in Orca's own order.
  const tabsScan = await orca.listTerminals();
  const tabs = tabsScan.ok ? tabsScan.rows : [];
  if (!tabsScan.ok) {
    errors.push({ scope: "orca", axis: "tabs", code: tabsScan.code, message: tabsScan.message });
  }

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
    scannedAt,
    ok: tabsScan.ok,
    errors,
    tabs,
    roots,
    strayTabs,
    rows,
    summary: summarize({ roots, tabs, rows, strayTabs }),
  };
}
