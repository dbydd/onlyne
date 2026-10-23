// The supervisor board: two independent axes, joined only where a title says so.
//
// Axis A — Orca tabs. One flat `orca terminal list --json` call, then cut down
// to the panes a connected session says it runs in. A session's own process
// reports the pane it was spawned in over the adapter protocol
// (`observed.host.orca.pane_key`, `crates/onlyne-session/src/host.rs`), and that
// report is the whole authority: this board reads no cache file and derives no
// scope from a worktree, so a tab of another worktree, another tool or another
// agent can never be listed. Nothing connected means an empty tab axis, never a
// permissive one.
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
// root, and a CLI that does not know the verb each report themselves, and a
// session that reports no pane simply does not scope one. Nothing here throws
// and nothing here writes.

export const UNKNOWN_ROLE = "(unknown role)";
export const TITLE_PREFIX = "onlyne:";

/**
 * Restrict the tab axis to the connected pi panes.
 *
 * A pane key is `<tab_id>:<leaf_id>` on both sides — Orca's own `terminal list`
 * spelling and the key a session reports — so the cut is a set intersection and
 * nothing else. Every tab left out is somebody else's: another worktree's pi,
 * another tool's terminal, or a pane whose pi has not reported in.
 *
 * Nothing bound is a real answer, not a failure: the tab axis is then empty and
 * `source: "none"` says the board is waiting for pi rather than misconfigured.
 *
 * @param {Array} tabs rows from `orca terminal list`, normalized
 * @param {Iterable<string>} boundPanes pane keys of live, reporting sessions
 * @returns {{tabs: Array, scope: {source: "connected"|"none", panes: number, hidden: number}}}
 */
export function scopeTabs(tabs, boundPanes) {
  const bound = new Set(boundPanes ?? []);
  const kept = tabs.filter((tab) => tab.paneKey && bound.has(tab.paneKey));
  return {
    tabs: kept,
    scope: {
      source: kept.length ? "connected" : "none",
      panes: kept.length,
      hidden: tabs.length - kept.length,
    },
  };
}

/**
 * The Orca pane a live session says it runs in, or null.
 *
 * "Live" is the projection's own verdict — anything but `public_lifecycle:
 * exited`, which the reducer derives from `agent: gone` and from a settled
 * completion. It is deliberately the only liveness this board judges by: the
 * client's reconcile loop is what turns a dead pane into `exited`
 * (`crates/onlyne-session/src/reconcile.rs`), and a second opinion here would
 * be a second source of truth about one fact.
 *
 * @param {{lifecycle?: string|null, host?: {paneKey?: string|null}|null}} session
 */
export function livePaneOf(session) {
  if (!session?.host?.paneKey) return null;
  if (session.lifecycle === "exited") return null;
  return session.host.paneKey;
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
 * Read both axes and return the board.
 *
 * Axis B runs first: the tab axis is scoped by what the sessions say, so the
 * session axis has to be read before any tab can be kept or hidden.
 *
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
  const allTabs = tabsScan.ok ? tabsScan.rows : [];
  if (!tabsScan.ok) {
    errors.push({ scope: "orca", axis: "tabs", code: tabsScan.code, message: tabsScan.message });
  }

  // Axis B: one `sessions` + `roles` pair per root. The panes the live sessions
  // report are the scope of the tab axis, so this read comes before the cut.
  const bound = new Set();
  const scans = [];
  for (const root of serverRoots) {
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
    const sessions = sessionsScan.ok ? sessionsScan.sessions : [];
    for (const session of sessions) {
      const paneKey = livePaneOf(session);
      if (paneKey) bound.add(paneKey);
    }
    scans.push({ root, sessionsScan, rolesScan, failures, sessions });
  }

  const { tabs, scope } = scopeTabs(allTabs, bound);

  // The join: title -> task, over the tabs that survived the cut.
  const tabsByTask = indexTabsByTask(tabs);
  const claimed = new Set();
  const roots = [];
  for (const entry of scans) {
    const roles = entry.rolesScan.ok ? entry.rolesScan.roles : [];
    const groups = new Map();
    for (const role of roles) {
      groups.set(role.role, { role: role.role, presence: role, rows: [] });
    }
    let sessionsWorking = 0;
    for (const session of entry.sessions) {
      const role = session.role ?? UNKNOWN_ROLE;
      let group = groups.get(role);
      if (!group) {
        group = { role, presence: null, rows: [] };
        groups.set(role, group);
      }
      const row = taskRow({
        root: entry.root,
        role,
        session,
        tab: claimTab(tabsByTask, claimed, session.taskId),
      });
      if (isWorking(row)) sessionsWorking += 1;
      group.rows.push(row);
    }

    const sections = sortedGroups(groups);
    roots.push({
      root: entry.root,
      sessionsScan: scanView(entry.sessionsScan, "sessions"),
      rolesScan: scanView(entry.rolesScan, "roles"),
      roles,
      failures: entry.failures,
      groups: sections,
      summary: { roles: sections.length, sessions: entry.sessions.length, sessionsWorking },
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
    totalTabs: allTabs.length,
    scope,
    roots,
    strayTabs,
    rows,
    summary: summarize({ roots, tabs, allTabs, rows, strayTabs }),
  };
}
