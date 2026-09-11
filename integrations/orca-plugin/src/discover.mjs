// Zero-config discovery: registered Orca worktrees -> role workspaces ->
// mapping rows -> live terminals -> backend session state.
//
// A worktree is a role workspace when `<path>/.onlyne/cache/orca-tabs.jsonl`
// exists. That file is the only source of task/pane identity (the plugin never
// writes it), `orca terminal list` is the only source of liveness, and the
// workspace-local onlyne client socket is the only source of session state.
// Every one of those can be absent; each absence degrades that axis instead of
// failing the scan.

import { existsSync } from "node:fs";
import { readMapping as defaultReadMapping } from "./mapping.mjs";
import { clientSocketPath } from "./onlyne-cli.mjs";
import { worktreeSelector } from "./orca-cli.mjs";

/** Removed worktree groups stay visible (greyed) for this long. */
export const GRAVEYARD_TTL_MS = 10 * 60 * 1000;
export const TERMINAL_SCAN_CONCURRENCY = 4;

function isMissingMapping(result) {
  return !result || result.missing === true || result.ok === false;
}

/** One path can appear twice (main worktree + attached workspace entry). */
export function dedupeWorktrees(rows) {
  const byPath = new Map();
  for (const row of rows) {
    const existing = byPath.get(row.path);
    if (!existing) {
      byPath.set(row.path, row);
      continue;
    }
    if (existing.isBare && !row.isBare) byPath.set(row.path, row);
  }
  return [...byPath.values()];
}

async function mapLimit(items, limit, worker) {
  const results = new Array(items.length);
  let next = 0;
  const runners = Array.from({ length: Math.min(limit, items.length) }, async () => {
    while (next < items.length) {
      const index = next++;
      results[index] = await worker(items[index], index);
    }
  });
  await Promise.all(runners);
  return results;
}

/**
 * Match mapping rows against session state on the identity both sides carry:
 * task id first, then session id. No role-wide guessing — a role runs several
 * tasks, and a wrong join would mislabel a tab.
 */
export function indexSessions(sessions) {
  const byTask = new Map();
  const bySession = new Map();
  for (const session of sessions) {
    byTask.set(session.taskId, session);
    if (session.sessionId) bySession.set(session.sessionId, session);
  }
  return {
    lookup(row) {
      if (row.taskId && byTask.has(row.taskId)) return byTask.get(row.taskId);
      if (row.sessionId && bySession.has(row.sessionId)) return bySession.get(row.sessionId);
      return null;
    },
  };
}

function indexTerminals(rows) {
  const byPane = new Map();
  const byHandle = new Map();
  for (const row of rows) {
    if (row.paneKey) byPane.set(row.paneKey, row);
    byHandle.set(row.handle, row);
  }
  return {
    byPane,
    byHandle,
    lookup(row) {
      if (row.paneKey && byPane.has(row.paneKey)) return byPane.get(row.paneKey);
      if (row.handle && byHandle.has(row.handle)) return byHandle.get(row.handle);
      return null;
    },
  };
}

export function groupRows(rows) {
  const groups = new Map();
  for (const row of rows) {
    const key = row.role ?? "(unknown role)";
    const bucket = groups.get(key);
    if (bucket) bucket.rows.push(row);
    else groups.set(key, { role: key, rows: [row] });
  }
  const list = [...groups.values()];
  for (const group of list) {
    group.liveTabs = group.rows.filter((row) => row.live).length;
    group.sessionsWorking = group.rows.filter(isWorking).length;
    group.removed = group.rows.every((row) => row.removed);
  }
  list.sort((left, right) => left.role.localeCompare(right.role));
  return list;
}

export function isWorking(row) {
  const lifecycle = row.session?.lifecycle;
  const agent = row.session?.agent;
  return lifecycle === "working" || agent === "running";
}

export function summarize(rows) {
  const roles = new Set();
  let liveTabs = 0;
  let sessionsWorking = 0;
  let removed = 0;
  for (const row of rows) {
    roles.add(row.role ?? "(unknown role)");
    if (row.live) liveTabs += 1;
    if (isWorking(row)) sessionsWorking += 1;
    if (row.removed) removed += 1;
  }
  return { roles: roles.size, liveTabs, sessionsWorking, removed, rows: rows.length };
}

/**
 * @param {object} options
 * @param {object} options.orca   createOrcaCli()
 * @param {object} options.onlyne createOnlyneCli()
 * @param {Function} [options.readMapping]
 * @param {Map} [options.graveyard] path -> {rows, removedAt, worktree}
 * @param {Function} [options.now]
 * @param {Function} [options.exists]
 */
export async function collectBoard({
  orca,
  onlyne,
  readMapping = defaultReadMapping,
  graveyard = new Map(),
  now = () => Date.now(),
  exists = existsSync,
} = {}) {
  const scannedAt = now();
  const errors = [];
  const listed = await orca.listWorktrees();
  if (!listed.ok) {
    return {
      scannedAt,
      ok: false,
      errors: [{ scope: "orca", code: listed.code, message: listed.message }],
      workspaces: [],
      rows: [],
      groups: [],
      summary: summarize([]),
    };
  }

  const candidates = dedupeWorktrees(listed.rows);
  const roleWorkspaces = [];
  for (const worktree of candidates) {
    const mapping = readMapping(worktree.path);
    if (isMissingMapping(mapping)) {
      if (mapping.error) {
        errors.push({ scope: worktree.path, code: "mapping_read", message: mapping.error });
      }
      continue;
    }
    roleWorkspaces.push({ worktree, mapping });
  }

  const scans = await mapLimit(roleWorkspaces, TERMINAL_SCAN_CONCURRENCY, async ({ worktree, mapping }) => {
    const selector = worktreeSelector(worktree.path);
    const terminals = await orca.listTerminals(selector);
    let sessions = { ok: false, code: "no_socket", message: "no client socket in this workspace" };
    const socketPath = clientSocketPath(worktree.path);
    if (exists(socketPath)) {
      sessions = await onlyne.querySessions(socketPath);
    }
    return { worktree, mapping, selector, terminals, sessions };
  });

  const rows = [];
  const workspaces = [];
  for (const scan of scans) {
    const { worktree, mapping, selector, terminals, sessions } = scan;
    const sessionSource = sessions.ok ? "onlyne" : "mapping";
    if (!terminals.ok) {
      errors.push({ scope: worktree.path, code: terminals.code, message: terminals.message });
    } else if (!sessions.ok) {
      errors.push({ scope: worktree.path, code: sessions.code, message: sessions.message });
    }
    const terminalIndex = terminals.ok ? indexTerminals(terminals.rows) : null;
    const sessionIndex = sessions.ok ? indexSessions(sessions.sessions) : null;
    const workspaceView = {
      path: worktree.path,
      selector,
      worktreeId: worktree.worktreeId,
      displayName: worktree.displayName,
      isBare: worktree.isBare,
      mappingPath: mapping.path,
      malformed: mapping.malformed,
      tombstones: mapping.tombstones.length,
      terminalScan: terminals.ok
        ? { ok: true, count: terminals.rows.length }
        : { ok: false, code: terminals.code, message: terminals.message },
      sessionScan: sessions.ok
        ? { ok: true, source: "onlyne", count: sessions.sessions.length }
        : { ok: false, source: "mapping", code: sessions.code, message: sessions.message },
      roles: [],
      liveTabs: 0,
      removed: false,
    };
    for (const mappingRow of mapping.rows) {
      const terminal = terminalIndex ? terminalIndex.lookup(mappingRow) : null;
      const row = {
        role: mappingRow.role,
        taskId: mappingRow.taskId,
        sessionId: mappingRow.sessionId,
        paneKey: mappingRow.paneKey,
        handle: mappingRow.handle,
        mappingState: mappingRow.state,
        title: mappingRow.title,
        updatedAt: mappingRow.updatedAt,
        worktreePath: worktree.path,
        selector,
        displayName: worktree.displayName,
        worktreeId: worktree.worktreeId,
        mappingPath: mapping.path,
        terminal,
        session: sessionIndex ? sessionIndex.lookup(mappingRow) : null,
        sessionSource,
        removed: false,
      };
      row.live = Boolean(terminal) && terminal.connected === true && mappingRow.state === "spawned";
      rows.push(row);
      if (row.role && !workspaceView.roles.includes(row.role)) workspaceView.roles.push(row.role);
      if (row.live) workspaceView.liveTabs += 1;
    }
    workspaceView.roles.sort();
    workspaces.push(workspaceView);
  }

  // Removed worktrees keep their last known rows, greyed, for a bounded while.
  const livePaths = new Set(workspaces.map((workspace) => workspace.path));
  for (const [path, entry] of graveyard) {
    if (scannedAt - entry.removedAt > GRAVEYARD_TTL_MS) {
      graveyard.delete(path);
      continue;
    }
    if (livePaths.has(path)) {
      graveyard.delete(path);
      continue;
    }
    for (const row of entry.rows) {
      rows.push({ ...row, removed: true, live: false, terminal: null });
    }
    workspaces.push({
      path,
      selector: worktreeSelector(path),
      worktreeId: entry.worktreeId ?? null,
      displayName: entry.displayName ?? null,
      mappingPath: null,
      malformed: 0,
      tombstones: 0,
      terminalScan: { ok: false, code: "worktree_removed", message: "worktree was removed" },
      sessionScan: { ok: false, source: "mapping", code: "worktree_removed", message: "worktree was removed" },
      roles: [...new Set(entry.rows.map((row) => row.role).filter(Boolean))].sort(),
      liveTabs: 0,
      removed: true,
    });
  }

  const groups = groupRows(rows);
  return {
    scannedAt,
    ok: true,
    errors,
    workspaces,
    rows,
    groups,
    summary: summarize(rows),
  };
}

/** Record a removed worktree's rows from the last board so it renders greyed. */
export function markRemoved(board, path, { now = () => Date.now() } = {}) {
  if (!board || !path) return null;
  const rows = board.rows.filter((row) => row.worktreePath === path);
  const workspace = board.workspaces.find((entry) => entry.path === path);
  if (!rows.length && !workspace) return null;
  return {
    rows,
    removedAt: now(),
    worktreeId: workspace?.worktreeId ?? rows[0]?.worktreeId ?? null,
    displayName: workspace?.displayName ?? rows[0]?.displayName ?? null,
  };
}
