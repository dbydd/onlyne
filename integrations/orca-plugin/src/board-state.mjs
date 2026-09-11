// Scan orchestration: event-driven rescans (debounced), a periodic fallback,
// and change detection that pushes the board to the user.
//
// pluginApi v1 gives a worker no way to push into a panel (no panel lifecycle
// events, no panel data channel) and this worker cannot know when a panel is
// open, so the fallback cadence runs while the plugin is enabled and the board
// is delivered through notifications + the plugin log pane instead.

import { formatBoard } from "./render.mjs";
import { markRemoved as markRemovedEntry } from "./discover.mjs";

export const EVENT_DEBOUNCE_MS = 2000;
export const FALLBACK_CADENCE_MS = 5000;
export const NOTIFY_COOLDOWN_MS = 30000;

/** Structure only: task/live/removed shape, not session churn. */
export function structuralFingerprint(board) {
  if (!board) return "none";
  const rows = [...board.rows]
    .map((row) => `${row.role ?? "?"}|${row.taskId ?? "?"}|${row.paneKey ?? "?"}|${row.live ? 1 : 0}|${row.removed ? 1 : 0}`)
    .sort();
  return JSON.stringify([board.summary.roles, board.summary.liveTabs, rows]);
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

  const graveyard = new Map();
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
        board = await collect({ graveyard, now });
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
    log(`board changed (${reason}): ${board.summary.roles} roles · ${board.summary.liveTabs} live tabs · ${board.summary.sessionsWorking} working`);
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

  function markRemoved(path) {
    const entry = markRemovedEntry(board, path, { now });
    if (entry) graveyard.set(path, entry);
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
    markRemoved,
    getBoard: () => board,
    onBoard: (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    graveyard,
  };
}
