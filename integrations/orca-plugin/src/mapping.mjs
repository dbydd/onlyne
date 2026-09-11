// Reader for the backend's append-only tab mapping file.
//
// Contract (written by onlyne-client, read-only here):
//   <role workspace>/.onlyne/cache/orca-tabs.jsonl
//   one JSON object per line:
//   {"pane_key":"<tabId:leafId>","handle":"term_<uuid>","task_id":"…",
//    "session_id":"…","role":"planner","worktree_selector":"path:/abs/ws",
//    "title":"onlyne:<task_id>","state":"spawned|closed","updated_at":"<rfc3339>"}
//
// Later lines win per pane_key; `state: "closed"` is a tombstone. The file is
// absent for backends without Orca tabs (fake/zellij), which is not an error.

import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";

export const MAPPING_RELATIVE_PATH = ".onlyne/cache/orca-tabs.jsonl";
export const CLOSED_STATE = "closed";

export function mappingPath(workspacePath) {
  return join(workspacePath, MAPPING_RELATIVE_PATH);
}

function str(value) {
  return typeof value === "string" && value.length > 0 ? value : null;
}

export function normalizeMappingRow(raw) {
  const paneKey = str(raw.pane_key);
  if (!paneKey) return null;
  const state = str(raw.state);
  return {
    paneKey,
    handle: str(raw.handle),
    taskId: str(raw.task_id),
    sessionId: str(raw.session_id),
    role: str(raw.role),
    worktreeSelector: str(raw.worktree_selector),
    title: str(raw.title),
    state,
    updatedAt: str(raw.updated_at),
    closed: state === CLOSED_STATE,
  };
}

/**
 * Fold the append-only file into current state.
 * @returns {{rows: Array, malformed: number, tombstones: Array}}
 */
export function parseMapping(text) {
  const byPane = new Map();
  let malformed = 0;
  for (const line of String(text ?? "").split("\n")) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    let parsed;
    try {
      parsed = JSON.parse(trimmed);
    } catch {
      malformed += 1;
      continue;
    }
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      malformed += 1;
      continue;
    }
    const row = normalizeMappingRow(parsed);
    if (!row) {
      malformed += 1;
      continue;
    }
    // Map preserves first-seen order; re-inserting keeps the original slot so a
    // later line overrides the value without shuffling the board.
    byPane.set(row.paneKey, row);
  }
  const rows = [];
  const tombstones = [];
  for (const row of byPane.values()) {
    if (row.closed) tombstones.push(row);
    else rows.push(row);
  }
  return { rows, tombstones, malformed };
}

/**
 * @returns {{ok: boolean, missing: boolean, path: string, rows: Array,
 *            tombstones: Array, malformed: number, error?: string}}
 */
export function readMapping(
  workspacePath,
  { exists = existsSync, readFile = readFileSync } = {}
) {
  const path = mappingPath(workspacePath);
  if (!exists(path)) {
    return { ok: false, missing: true, path, rows: [], tombstones: [], malformed: 0 };
  }
  let text;
  try {
    text = readFile(path, "utf8");
  } catch (error) {
    return {
      ok: false,
      missing: false,
      path,
      rows: [],
      tombstones: [],
      malformed: 0,
      error: String(error?.message ?? error),
    };
  }
  const parsed = parseMapping(text);
  return { ok: true, missing: false, path, ...parsed };
}

