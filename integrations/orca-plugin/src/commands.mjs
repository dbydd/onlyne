// Command handlers. The command palette cannot pass arguments to a plugin
// command (`plugins.invokeCommand` can), so every handler accepts an optional
// `args.task` prefix and otherwise falls back to the unique-match rule.

import { writeFileSync } from "node:fs";
import { boardPayload } from "./panel-document.mjs";
import {
  NOTIFICATION_BODY_LIMIT,
  formatAgentContext,
  formatBoard,
  shortPaneKey,
  summaryLine,
} from "./render.mjs";

/** Where `onlyne-sessions.debug-board` drops the board the panel renders. */
export const DEBUG_BOARD_PATH = "/tmp/onlyne-board.json";

export const AMBIGUOUS_EXAMPLE_LIMIT = 5;

/**
 * Match rows on the two prefixes an operator can copy off the board: a task id,
 * or a pane prefix `<tabId>:<leafId>` (the shortened `tab8:leaf8` form the board
 * prints is accepted too). Without a prefix the pool narrows to live rows, so
 * the palette's argument-less invocation acts only when exactly one qualifies.
 *
 * @returns {{status: 'unique'|'none'|'ambiguous', matches: Array, prefix: string}}
 */
export function matchRowsByPrefix(rows, prefix) {
  const needle = String(prefix ?? "").trim().toLowerCase();
  const pool = rows.filter((row) => !row.removed);
  if (!needle) {
    const live = pool.filter((row) => row.live);
    if (!live.length) return { status: "none", matches: [], prefix: needle };
    return live.length === 1
      ? { status: "unique", matches: live, prefix: needle }
      : { status: "ambiguous", matches: live, prefix: needle };
  }
  const matches = pool.filter((row) => matchesPrefix(row, needle));
  if (!matches.length) return { status: "none", matches, prefix: needle };
  return matches.length === 1
    ? { status: "unique", matches, prefix: needle }
    : { status: "ambiguous", matches, prefix: needle };
}

function matchesPrefix(row, needle) {
  if (typeof row.taskId === "string" && row.taskId.toLowerCase().startsWith(needle)) return true;
  if (typeof row.paneKey !== "string" || !row.paneKey) return false;
  if (row.paneKey.toLowerCase().startsWith(needle)) return true;
  return shortPaneKey(row.paneKey).toLowerCase().startsWith(needle);
}

function candidateList(matches, limit = AMBIGUOUS_EXAMPLE_LIMIT) {
  const lines = matches.slice(0, limit).map((row) => {
    const label = row.taskId ?? shortPaneKey(row.paneKey);
    const parts = [`  ${label}`];
    if (row.role) parts.push(`(${row.role})`);
    if (row.root) parts.push(`@${row.root}`);
    return parts.join(" ");
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
export function createCommands({
  getBoard,
  refreshBoard,
  orca,
  notify,
  log = () => {},
  now = () => Date.now(),
  panelInfo = () => null,
  writeFile = writeFileSync,
}) {
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
    const match = matchRowsByPrefix(current?.rows ?? [], args?.task);
    if (match.status === "none") {
      const why = match.prefix
        ? `没有行的 task 或 pane_key 以 "${match.prefix}" 开头。`
        : "没有唯一的活 tab——给一个 task 或 pane 前缀。";
      await notify("Onlyne sessions: 跳转失败", `${why}\n看板：跑 onlyne-sessions.board`);
      return { ok: false, code: "no_match", message: why };
    }
    if (match.status === "ambiguous") {
      const why = `${match.matches.length} 行命中${match.prefix ? ` 前缀 "${match.prefix}"` : ""}，给更长的前缀：`;
      await notify("Onlyne sessions: 跳转失败", `${why}\n${candidateList(match.matches)}`);
      return { ok: false, code: "ambiguous", message: why, matches: match.matches.length };
    }
    const row = match.matches[0];
    if (!row.handle) {
      const why = "该行没有 handle（tab 轴没列出这个 pane），无法切换。";
      await notify("Onlyne sessions: 跳转失败", why);
      return { ok: false, code: "no_handle", message: why };
    }
    const switched = await orca.switchTerminal(row.handle);
    if (!switched.ok) {
      const why = `切换失败：${switched.code} — ${switched.message}`;
      await notify("Onlyne sessions: 跳转失败", why);
      return { ok: false, code: switched.code, message: why };
    }
    await notify("Onlyne sessions", `已切到 ${row.taskId ?? shortPaneKey(row.paneKey)}（${row.role ?? "tab"}）`);
    return { ok: true, taskId: row.taskId, handle: row.handle, paneKey: row.paneKey, selector: row.selector };
  }

  async function copyAgentContext(args = {}) {
    const current = await currentBoard();
    const match = matchRowsByPrefix(current?.rows ?? [], args?.task);
    if (match.status === "none") {
      const why = "没有匹配的 tab——给一个 task 或 pane 前缀。";
      await notify("Onlyne sessions: 复制上下文失败", why);
      return { ok: false, code: "no_match", message: why };
    }
    if (match.status === "ambiguous") {
      const why = `${match.matches.length} 行命中，给更长的前缀：`;
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

  /**
   * Drop the board the panel renders onto disk, for eyeballing or diffing from
   * a shell. The payload is `boardPayload` — the very object embedded in the
   * panel document — so the file and the panel cannot disagree.
   */
  async function debugBoard(args = {}) {
    const current = await currentBoard({ force: args?.force === true });
    const path =
      typeof args?.path === "string" && args.path.trim() ? args.path.trim() : DEBUG_BOARD_PATH;
    const payload = boardPayload(current, { generatedAt: now(), panel: panelInfo() });
    try {
      writeFile(path, `${JSON.stringify(payload, null, 2)}\n`, "utf8");
    } catch (error) {
      const why = `写 ${path} 失败：${error?.message ?? error}`;
      await notify("Onlyne sessions: debug board 失败", why);
      return { ok: false, code: "write_failed", message: why };
    }
    const line = summaryLine(current);
    log(`debug board → ${path} · ${line}`);
    await notify("Onlyne sessions: debug board", `${path}\n${line}`);
    return { ok: true, path, summary: current?.summary ?? null, generatedAt: payload.generatedAt };
  }

  return { refresh, board, debugBoard, focus, copyAgentContext };
}
