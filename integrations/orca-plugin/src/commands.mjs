// Command handlers. The command palette cannot pass arguments to a plugin
// command (`plugins.invokeCommand` can), so every handler accepts an optional
// `args.task` prefix and otherwise falls back to the unique-match rule.

import { NOTIFICATION_BODY_LIMIT, formatAgentContext, formatBoard } from "./render.mjs";

export const AMBIGUOUS_EXAMPLE_LIMIT = 5;

/** @returns {{status: 'unique'|'none'|'ambiguous', matches: Array, prefix: string}} */
export function matchRowsByTaskPrefix(rows, prefix) {
  const needle = String(prefix ?? "").trim().toLowerCase();
  const pool = rows.filter((row) => !row.removed);
  if (!needle) {
    const live = pool.filter((row) => row.live);
    return {
      status: live.length === 1 ? "unique" : live.length === 0 ? "none" : "ambiguous",
      matches: live,
      prefix: "",
    };
  }
  const matches = pool.filter(
    (row) => typeof row.taskId === "string" && row.taskId.toLowerCase().startsWith(needle)
  );
  return {
    status: matches.length === 1 ? "unique" : matches.length === 0 ? "none" : "ambiguous",
    matches,
    prefix: needle,
  };
}

function candidateList(matches, limit = AMBIGUOUS_EXAMPLE_LIMIT) {
  const lines = matches.slice(0, limit).map((row) => {
    const task = row.taskId ? row.taskId.slice(0, 12) : "(no task)";
    const state = row.live ? "live" : row.mappingState ?? "unknown";
    return `  ${task} · ${state} · ${row.paneKey ?? "—"}`;
  });
  if (matches.length > limit) lines.push(`  …(+${matches.length - limit})`);
  return lines.join("\n");
}

/**
 * @param {object} options
 * @param {Function} options.getBoard
 * @param {Function} options.refreshBoard
 * @param {object} options.orca createOrcaCli()
 * @param {Function} options.notify (title, body) => Promise<void>
 * @param {Function} [options.log]
 * @param {Function} [options.now]
 */
export function createCommands({ getBoard, refreshBoard, orca, notify, log = () => {}, now = () => Date.now() }) {
  async function currentBoard({ force = false } = {}) {
    const board = getBoard();
    if (board && !force) return board;
    return refreshBoard({ reason: "command" });
  }

  async function pushBoard(board) {
    const text = formatBoard(board, { now: now() });
    log(text);
    await notify("Onlyne sessions", formatBoard(board, { now: now(), limit: NOTIFICATION_BODY_LIMIT }));
    return text;
  }

  async function refresh(args = {}) {
    const board = await refreshBoard({ reason: args?.reason ?? "command" });
    await pushBoard(board);
    return { ok: true, board };
  }

  async function board(args = {}) {
    const current = await currentBoard({ force: args?.force === true });
    await pushBoard(current);
    return { ok: true, board: current };
  }

  async function focus(args = {}) {
    const current = await currentBoard();
    const match = matchRowsByTaskPrefix(current?.rows ?? [], args?.task);
    if (match.status === "none") {
      const why = match.prefix
        ? `没有 task 以 "${match.prefix}" 开头的活 tab。`
        : "没有唯一的活 tab——给一个 task 前缀。";
      await notify("Onlyne sessions: 跳转失败", `${why}\n看板：跑 onlyne-sessions.board`);
      return { ok: false, code: "no_match", message: why };
    }
    if (match.status === "ambiguous") {
      const why = `${match.matches.length} 个 tab 命中${match.prefix ? ` 前缀 "${match.prefix}"` : ""}，给更长的前缀：`;
      await notify("Onlyne sessions: 跳转失败", `${why}\n${candidateList(match.matches)}`);
      return { ok: false, code: "ambiguous", message: why, matches: match.matches.length };
    }
    const row = match.matches[0];
    if (!row.handle) {
      const why = "该行没有 handle（mapping 里缺字段），无法切换。";
      await notify("Onlyne sessions: 跳转失败", why);
      return { ok: false, code: "no_handle", message: why };
    }
    const switched = await orca.switchTerminal(row.handle);
    if (!switched.ok) {
      const why = `切换失败：${switched.code} — ${switched.message}`;
      await notify("Onlyne sessions: 跳转失败", why);
      return { ok: false, code: switched.code, message: why };
    }
    await notify("Onlyne sessions", `已切到 ${row.taskId ?? row.paneKey}（${row.role ?? "role"}）`);
    return { ok: true, taskId: row.taskId, handle: row.handle, paneKey: row.paneKey, selector: row.selector };
  }

  async function copyAgentContext(args = {}) {
    const current = await currentBoard();
    const match = matchRowsByTaskPrefix(current?.rows ?? [], args?.task);
    if (match.status === "none") {
      const why = "没有匹配的 tab——给一个 task 前缀。";
      await notify("Onlyne sessions: 复制上下文失败", why);
      return { ok: false, code: "no_match", message: why };
    }
    if (match.status === "ambiguous") {
      const why = `${match.matches.length} 个 tab 命中，给更长的前缀：`;
      await notify("Onlyne sessions: 复制上下文失败", `${why}\n${candidateList(match.matches)}`);
      return { ok: false, code: "ambiguous", message: why, matches: match.matches.length };
    }
    const row = match.matches[0];
    // pluginApi v1 has no clipboard host method; the notification is the
    // fallback the user asked for, and the structured result carries the triple
    // for callers that invoke the command through plugins.invokeCommand.
    const text = formatAgentContext(row);
    log(text);
    await notify("Onlyne sessions: agent 上下文（无剪贴板 API，请手动复制）", text);
    return { ok: true, context: text, taskId: row.taskId, paneKey: row.paneKey, handle: row.handle, selector: row.selector };
  }

  return { refresh, board, focus, copyAgentContext };
}
