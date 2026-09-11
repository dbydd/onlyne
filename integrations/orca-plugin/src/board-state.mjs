// Scan orchestration: event-driven rescans (debounced), a periodic fallback,
// and change detection that pushes the board to the user.
//
// pluginApi v1 gives a worker no way to push into a panel (no panel lifecycle
// events, no panel data channel) and this worker cannot know when a panel is
// open, so the fallback cadence runs while the plugin is enabled and the board
// is delivered through notifications + the plugin log pane instead.

import { formatBoard } from "./render.mjs";

export const EVENT_DEBOUNCE_MS = 2000;
export const FALLBACK_CADENCE_MS = 5000;
export const NOTIFY_COOLDOWN_MS = 30000;

/**
 * Structure only: which rows exist where (root/role/task/pane/liveness) plus
 * which axis is down. Session churn inside one row is deliberately invisible.
 */
export function structuralFingerprint(board) {
  if (!board) return "none";
  const rows = [...board.rows]
    .map(
      (row) =>
        `${row.kind}|${row.root ?? "?"}|${row.role ?? "?"}|${row.taskId ?? "?"}|${row.paneKey ?? "?"}|${row.live ? 1 : 0}`
    )
    .sort();
  const errors = [...board.errors].map((error) => `${error.scope}|${error.axis}|${error.code}`).sort();
  return JSON.stringify([board.summary.roots, board.summary.roles, errors, rows]);
}

export function createBoardState({
  collect,
  notify,
  log = () => {},
  now = () => Date.now(),
  timers = {},
  cadenceMs = FALLBACK_CADENCE_MS,
  debounceMs = EVENT_DEBOUNCE_MS,
  notifyCooldownMs = NOTIFY_COOLDOWN_MS,
} = {}) {
  const setTimeoutFn = timers.setTimeout ?? setTimeout;
  const clearTimeoutFn = timers.clearTimeout ?? clearTimeout;
  const setIntervalFn = timers.setInterval ?? setInterval;
  const clearIntervalFn = timers.clearInterval ?? clearInterval;

  const listeners = new Set();
  let board = null;
  let inFlight = null;
  let debounceTimer = null;
  let cadenceTimer = null;
  let fingerprint = null;
  let lastNotifiedAt = 0;
  let stopped = false;

  async function scan({ reason }) {
    if (inFlight) return inFlight;
    inFlight = (async () => {
      try {
        board = await collect({ now });
        await publish(reason);
        return board;
      } catch (error) {
        log(`scan failed (${reason}): ${error?.message ?? error}`);
        return board;
      } finally {
        inFlight = null;
      }
    })();
    return inFlight;
  }

  async function publish(reason) {
    const next = structuralFingerprint(board);
    const changed = fingerprint !== null && next !== fingerprint;
    fingerprint = next;
    for (const listener of listeners) listener(board, { reason, changed });
    if (!changed) return;
    log(
      `board changed (${reason}): ${board.summary.roots} roots · ${board.summary.roles} roles · ` +
        `${board.summary.tabs} tabs (${board.summary.liveTabs} live)`
    );
    if (now() - lastNotifiedAt < notifyCooldownMs) return;
    lastNotifiedAt = now();
    try {
      await notify("Onlyne sessions", formatBoard(board, { now: now() }));
    } catch (error) {
      log(`notification failed: ${error?.message ?? error}`);
    }
  }

  function scheduleRefresh({ reason = "event", delay = debounceMs } = {}) {
    if (stopped) return;
    if (debounceTimer) clearTimeoutFn(debounceTimer);
    debounceTimer = setTimeoutFn(() => {
      debounceTimer = null;
      void scan({ reason });
    }, delay);
    if (typeof debounceTimer?.unref === "function") debounceTimer.unref();
  }

  function start() {
    stopped = false;
    void scan({ reason: "activate" });
    cadenceTimer = setIntervalFn(() => {
      void scan({ reason: "cadence" });
    }, cadenceMs);
    if (typeof cadenceTimer?.unref === "function") cadenceTimer.unref();
    return cadenceTimer;
  }

  function stop() {
    stopped = true;
    if (debounceTimer) clearTimeoutFn(debounceTimer);
    if (cadenceTimer) clearIntervalFn(cadenceTimer);
    debounceTimer = null;
    cadenceTimer = null;
  }

  return {
    start,
    stop,
    refresh: (options = {}) => scan({ reason: options.reason ?? "manual" }),
    scheduleRefresh,
    getBoard: () => board,
    onBoard: (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
}
