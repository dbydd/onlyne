// The other question a background extension makes necessary: is work this
// session dispatched through the `Agent` tool still running where the agent loop
// cannot see it?
//
// The subagents extension is not one of the `pi-background-tasks` family, and
// its `Agent` call is not one of those tools. A field test on one session showed
// all five `Agent` calls issued in a single assistant message, each returning at
// once with `Agent started in background` and an output file under
// `/tmp/pi-subagents-<uid>/` — so the tool hands back a handle and the child
// carries on, and no `tool_execution_start` / `tool_execution_end` pair brackets
// it. The tool is not pi's own either: the string does not appear in pi's dist,
// and pi's extension API exposes no subagent or background-task query at all,
// only `getActiveTools` / `getAllTools` / `setActiveTools`. So there is no event
// to subscribe to and no frame to ask for; what the extension leaves behind is
// an on-disk registry at `~/.pi/subagents/missions/<uuid>.json`, and this probe
// reads that.
//
// Every failure mode is inert, exactly as in `background-work.mjs`: no
// directory, an unreadable file, a missing field, malformed JSON, or a read that
// throws all read as "no subagent work known" and leave the plugin's own
// judgement untouched. A probe that throws is worse than one that answers
// nothing.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";

/**
 * Mission statuses that mean the work is over.
 *
 * The live spelling was never observed. The machine this was measured on had
 * four missions, all of them terminal (`completed`, `failed`, `cancelled` for
 * missions; `completed`, `failed` for `workflowChildren`; `stopped`, `complete`,
 * `failed` for `runs[].status`), so the running value is unenumerated. The safe
 * direction is therefore "anything not on this list is live": guessing the other
 * way would read a genuinely running session as idle, which is the exact bug
 * this probe exists to fix. The cost of being wrong this way is a session that
 * waits a little longer than it had to — a new terminal spelling delays the
 * phase, it does not lose work.
 */
export const TERMINAL_MISSION_STATUSES = Object.freeze([
  "cancelled",
  "complete",
  "completed",
  "failed",
  "stopped",
]);

/**
 * How many mission files one call will open. The registry is one JSON object per
 * mission and grows without bound over a machine's life, so an uncapped scan
 * would make the probe cost a function of history while the caller only ever
 * cares about the present. The cap trades that for a bounded read: a session
 * whose live work sits past the cap is reported as not running, the same fail-safe
 * direction as every other answer here, and the newest files are read first
 * because those are the ones just dispatched.
 */
export const DEFAULT_MAX_MISSION_FILES = 200;

/**
 * How many bytes one mission file may be before it is skipped. A mission
 * transcript, not a mission, is the large artefact; anything past this is not
 * the status record this probe reads.
 */
export const DEFAULT_MAX_MISSION_BYTES = 256 * 1024;

/** @param {unknown} status */
export function isTerminalMissionStatus(status) {
  return typeof status === "string" && TERMINAL_MISSION_STATUSES.includes(status);
}

/**
 * @param {unknown} mission
 * @param {string | null} sessionId this session's id, or `null` when unknown
 */
export function isLiveMission(mission, sessionId = null) {
  if (!mission || typeof mission !== "object") return false;
  // A mission with no status is not live: absence of evidence is absence of
  // work, the same direction `background-work.mjs` fails safe in.
  if (typeof mission.status !== "string" || !mission.status) return false;
  if (isTerminalMissionStatus(mission.status)) return false;
  const owner = mission.ownerSessionId;
  if (typeof sessionId === "string" && sessionId) {
    // Scope by owner so the answer is "is *this* session's work still running",
    // not "is anything on this machine running".
    return typeof owner === "string" && owner === sessionId;
  }
  // The caller cannot name its own session, so a mission naming no session still
  // counts: watching a silent subset would be worse than watching too much.
  return true;
}

/** Newest first, so a capped scan spends its budget on the missions just dispatched. */
function byNewestFirst(left, right) {
  const a = right.mtimeMs ?? 0;
  const b = left.mtimeMs ?? 0;
  if (a !== b) return a - b;
  return String(right.name).localeCompare(String(left.name));
}

/** @returns {string[]} the `.json` mission files, newest first and already capped. */
function missionFiles(directory, maxFiles) {
  const entries = fs.readdirSync(directory, { withFileTypes: true });
  return entries
    .filter((entry) => entry.isFile() && entry.name.endsWith(".json"))
    .map((entry) => {
      let mtimeMs = 0;
      try {
        mtimeMs = fs.statSync(path.join(directory, entry.name)).mtimeMs;
      } catch {
        /* an entry that will not stat sorts as oldest and may be cut by the cap */
      }
      return { name: entry.name, mtimeMs };
    })
    .sort(byNewestFirst)
    .slice(0, maxFiles)
    .map((entry) => path.join(directory, entry.name));
}

function readMission(file, maxBytes) {
  if (fs.statSync(file).size > maxBytes) return null;
  return JSON.parse(fs.readFileSync(file, "utf8"));
}

/**
 * One live-mission question, asked of the on-disk registry and answered by
 * whatever the subagents extension left there. Inert by construction: a throw
 * anywhere inside resolves to `false`.
 *
 * @param {{
 *   homeDir?: string,
 *   getSessionId?: () => string | null,
 *   log?: (line: string) => void,
 *   maxFiles?: number,
 *   maxBytes?: number,
 * }} options
 */
export function createSubagentProbe({
  homeDir = os.homedir(),
  getSessionId = null,
  log = () => {},
  maxFiles = DEFAULT_MAX_MISSION_FILES,
  maxBytes = DEFAULT_MAX_MISSION_BYTES,
} = {}) {
  const warned = new Set();

  const warnOnce = (reason) => {
    if (warned.has(reason)) return;
    warned.add(reason);
    log(`subagent work: ${reason}`);
  };

  const sessionId = () => {
    if (typeof getSessionId !== "function") return null;
    return getSessionId() ?? null;
  };

  /**
   * @returns {Promise<boolean>}
   */
  function running() {
    try {
      const directory = path.join(homeDir, ".pi", "subagents", "missions");
      if (!fs.existsSync(directory)) {
        // No registry at all means the extension has never run here; that is a
        // steady state rather than a fault, so it is recorded once.
        warnOnce("no subagent registry in this home");
        return false;
      }
      for (const file of missionFiles(directory, maxFiles)) {
        let mission = null;
        try {
          mission = readMission(file, maxBytes);
        } catch (error) {
          warnOnce(`mission unreadable: ${error.message}`);
          continue;
        }
        if (mission === null) {
          warnOnce("mission record too large to be a status record");
          continue;
        }
        if (isLiveMission(mission, sessionId())) return true;
      }
      return false;
    } catch (error) {
      warnOnce(`registry unreadable: ${error.message}`);
      return false;
    }
  }

  return { running };
}
