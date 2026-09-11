// Board rendering: one text formatter shared by command results, desktop
// notifications (body ≤ 1000 chars) and the plugin log pane.

export const NOTIFICATION_BODY_LIMIT = 1000;
export const BOARD_TEXT_LIMIT = 950;

export const EMPTY_BOARD_NOTE =
  "还没有 serverRoots，也没有 Orca tab：在 ~/.config/onlyne-sessions/config.json 里加 serverRoots。";

export function summaryLine(board) {
  const summary = board?.summary ?? {
    roots: 0,
    roles: 0,
    tabs: 0,
    hiddenTabs: 0,
    liveTabs: 0,
    sessions: 0,
    sessionsWorking: 0,
  };
  const hidden = summary.hiddenTabs ? ` · ${summary.hiddenTabs} hidden` : "";
  return (
    `Onlyne sessions · ${summary.roots} roots · ${summary.roles} roles · ` +
    `${summary.tabs} tabs (${summary.liveTabs} live)${hidden} · ` +
    `${summary.sessions} sessions (${summary.sessionsWorking} working)`
  );
}

/** Epoch ms, epoch seconds (the admin rows' string `updated_at`), or RFC3339. */
export function toEpochMs(value) {
  if (typeof value === "number") return value < 1e11 ? value * 1000 : value;
  const text = String(value ?? "").trim();
  if (/^\d+$/.test(text)) {
    const seconds = Number(text);
    return seconds < 1e11 ? seconds * 1000 : seconds;
  }
  return Date.parse(text);
}

/**
 * `12s` / `3m` / `5h` / `2d` since a timestamp. Measured on 2026-09-11: a
 * `terminal list` row carries epoch milliseconds in `lastOutputAt`, while the
 * admin `sessions` row carries epoch *seconds* as a string in `updated_at`
 * (`"1789093578"`), and `Date.parse` reads that as NaN.
 */
export function relativeTime(value, now = Date.now()) {
  const ms = toEpochMs(value);
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

export function shortTask(taskId) {
  if (typeof taskId !== "string" || !taskId) return "(no task)";
  return taskId.length > 8 ? taskId.slice(0, 8) : taskId;
}

function shortId(value) {
  return typeof value === "string" && value ? value.slice(0, 8) : "—";
}

/** A session that ended badly reads as `✕`, whatever its tab says. */
export function isBadOutcome(row) {
  const outcome = row?.session?.outcome;
  return outcome === "fault" || outcome === "cancelled" || outcome === "failed";
}

/** Liveness is the tab's own `connected` flag; nothing else is consulted. */
export function rowGlyph(row) {
  if (isBadOutcome(row)) return "✕";
  return row.live ? "●" : "○";
}

export function rowState(row) {
  if (row.session) {
    const parts = [row.session.lifecycle, row.session.agent].filter(Boolean);
    if (row.session.outcome) parts.push(row.session.outcome);
    if (parts.length) return parts.join("/");
  }
  return "unknown";
}

export function rowLine(row, now = Date.now()) {
  if (row.kind === "tab") {
    const title = row.title?.trim();
    const parts = [`${rowGlyph(row)} tab`, title ? `title=${title}` : "(无标题)"];
    parts.push(relativeTime(row.lastOutputAt, now));
    parts.push(shortPaneKey(row.paneKey));
    parts.push(`wt ${shortId(row.worktreeId)}`);
    return parts.join(" · ");
  }
  const parts = [`${rowGlyph(row)} ${shortTask(row.taskId)}`, rowState(row)];
  if (!row.joined) parts.push("无 tab");
  parts.push(relativeTime(row.lastOutputAt ?? row.session?.updatedAt, now));
  parts.push(shortPaneKey(row.paneKey));
  return parts.join(" · ");
}

function rootHeadline(root) {
  const { roles, sessions, sessionsWorking } = root.summary;
  return `${roles} roles · ${sessions} sessions · ${sessionsWorking} working`;
}

function groupHeadline(group) {
  const presence = group.presence?.presence ?? "no role row";
  return `${presence} · ${group.rows.length} tasks · ${group.live} live`;
}

/**
 * The board text: header counts, one section per configured server root
 * (`!` lines for that root's own failures), then the unjoined tabs.
 */
export function formatBoard(board, { now = Date.now(), limit = BOARD_TEXT_LIMIT } = {}) {
  const lines = [summaryLine(board)];
  const roots = board?.roots ?? [];
  const stray = board?.strayTabs ?? [];

  for (const failure of board?.errors ?? []) {
    if (failure.scope !== "orca") continue;
    lines.push(`! tabs: ${failure.code} — ${failure.message}`);
  }
  for (const root of roots) {
    lines.push(`${root.root}  (${rootHeadline(root)})`);
    for (const failure of root.failures) {
      lines.push(`  ! ${failure.axis}: ${failure.code} — ${failure.message}`);
    }
    for (const group of root.groups) {
      lines.push(`  ${group.role}  (${groupHeadline(group)})`);
      for (const row of group.rows) lines.push(`    ${rowLine(row, now)}`);
    }
  }
  if (!roots.length && !stray.length) lines.push(EMPTY_BOARD_NOTE);
  if (stray.length) {
    lines.push(`未 join 的 tab (${stray.length})`);
    for (const row of stray) lines.push(`  ${rowLine(row, now)}`);
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

/**
 * The addressing strings a supervisor needs to re-attach to a tab. `selector`
 * is the tab row's own worktreeId, and the line is omitted when the CLI did not
 * report one; `task` is omitted for a tab that joined no session.
 */
export function formatAgentContext(row) {
  const lines = [];
  if (row.taskId) lines.push(`task: ${row.taskId}`);
  lines.push(`pane_key: ${row.paneKey ?? "—"}`);
  lines.push(`handle: ${row.handle ?? "—"}`);
  if (row.selector) lines.push(`orca selector: ${row.selector}`);
  return lines.join("\n");
}
