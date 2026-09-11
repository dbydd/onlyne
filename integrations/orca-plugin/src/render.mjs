// Board rendering: one text formatter shared by command results, desktop
// notifications (body ≤ 1000 chars) and the plugin log pane.

export const NOTIFICATION_BODY_LIMIT = 1000;
export const BOARD_TEXT_LIMIT = 950;

export const EMPTY_BOARD_NOTE =
  "还没有 role 工作区：等 supervisor 拉起 role client" +
  "（role 工作区里要出现 .onlyne/cache/orca-tabs.jsonl）。";

export function summaryLine(board) {
  const summary = board?.summary ?? { roles: 0, liveTabs: 0, sessionsWorking: 0 };
  return `Onlyne sessions · ${summary.roles} roles · ${summary.liveTabs} live tabs · ${summary.sessionsWorking} working`;
}

export function relativeTime(value, now = Date.now()) {
  const ms = typeof value === "number" ? value : Date.parse(String(value ?? ""));
  if (!Number.isFinite(ms)) return "—";
  const delta = Math.max(0, now - ms);
  const seconds = Math.floor(delta / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h`;
  return `${Math.floor(hours / 24)}d`;
}

/** `45e603f7-…:b6d067b6-…` -> `45e603f7:b6d067b6`; other shapes pass through. */
export function shortPaneKey(paneKey) {
  if (typeof paneKey !== "string" || !paneKey) return "—";
  const [tabId, leafId] = paneKey.split(":");
  if (!tabId || !leafId) return paneKey.slice(0, 12);
  return `${tabId.slice(0, 8)}:${leafId.slice(0, 8)}`;
}

export function rowGlyph(row) {
  if (row.removed) return "✕";
  return row.live ? "●" : "○";
}

export function rowState(row) {
  if (row.session) {
    const parts = [row.session.lifecycle, row.session.agent].filter(Boolean);
    if (row.session.outcome) parts.push(row.session.outcome);
    if (parts.length) return parts.join("/");
  }
  return row.mappingState ?? "unknown";
}

export function rowLine(row, now = Date.now()) {
  const parts = [`${rowGlyph(row)} ${shortTask(row.taskId)}`, rowState(row)];
  if (row.removed) parts.push("worktree 已移除");
  else parts.push(relativeTime(row.terminal?.lastOutputAt ?? row.updatedAt, now));
  parts.push(shortPaneKey(row.paneKey));
  return parts.join(" · ");
}

export function shortTask(taskId) {
  if (typeof taskId !== "string" || !taskId) return "(no task)";
  return taskId.length > 8 ? taskId.slice(0, 8) : taskId;
}

export function formatBoard(board, { now = Date.now(), limit = BOARD_TEXT_LIMIT } = {}) {
  const lines = [summaryLine(board)];
  const groups = board?.groups ?? [];
  if (!groups.length) {
    lines.push(EMPTY_BOARD_NOTE);
    return lines.join("\n");
  }
  for (const group of groups) {
    lines.push(`${group.role}  (${group.liveTabs}/${group.rows.length} live)`);
    for (const row of group.rows) lines.push(`  ${rowLine(row, now)}`);
  }
  return truncateLines(lines, limit);
}

function truncateLines(lines, limit) {
  const kept = [];
  let used = 0;
  for (const line of lines) {
    const cost = line.length + 1;
    if (used + cost > limit) {
      const remaining = lines.length - kept.length;
      kept.push(`…(+${remaining} 行)`);
      break;
    }
    kept.push(line);
    used += cost;
  }
  return kept.join("\n");
}

/** The three addressing strings a supervisor needs to re-attach to a tab. */
function agentContextTriple(row) {
  return {
    taskId: row.taskId ?? null,
    paneKey: row.paneKey ?? null,
    handle: row.handle ?? null,
    selector: row.selector ?? null,
  };
}

/** pluginApi v1 has no clipboard host method; notification text is the fallback. */
export function formatAgentContext(row) {
  const triple = agentContextTriple(row);
  return [
    `task: ${triple.taskId ?? "—"}`,
    `pane_key: ${triple.paneKey ?? "—"}`,
    `handle: ${triple.handle ?? "—"}`,
    `orca selector: ${triple.selector ?? "—"}`,
  ].join("\n");
}
