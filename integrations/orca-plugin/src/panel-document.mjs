// The board's only path into the Orca panel: the panel document itself.
//
// pluginApi v1 gives a sandboxed panel no way to reach its worker. The bridge
// admits exactly three host actions (`PLUGIN_PANEL_ACTIONS` in Orca's
// src/shared/plugins/plugin-host-api.ts:263 — `workspace.readContext`,
// `terminal.sendText`, `notifications.show`), the iframe CSP is
// `default-src 'none'; connect-src 'none'`
// (src/shared/plugins/plugin-panel-shell.ts:20), and the host posts nothing into
// the frame but watchdog pings and action results
// (src/renderer/src/components/right-sidebar/plugin-panel-bridge-host.ts). So
// the board travels as the panel's own document: this worker renders it into
// the panel entry, and Orca re-reads that entry from the plugin root whenever it
// opens or refreshes the panel
// (src/main/plugins/plugin-panel-controller.ts:142-148).
//
// Writing that file is sanctioned for a dev tree and refused for an installed
// one. `verifyHashAddressedPluginContent` returns ok the moment
// `contentHash === null` — "Dev trees are intentionally mutable; installed
// hash-addressed trees are not" (src/main/plugins/plugin-content-integrity.ts)
// — while a hash-addressed install is re-hashed on every panel load, so writing
// there would fail integrity and lose the panel entirely. The dev watcher turns
// the write into a reload on its own: any change under a configured dev path
// schedules the 300 ms debounced refresh
// (src/main/plugins/plugin-dev-watcher.ts:106-114), and the renderer remounts
// the frame when the entry HTML changed
// (src/renderer/src/components/right-sidebar/PluginPanel.tsx:143-147).
//
// Because the document *is* the payload it carries a snapshot, not a fetch:
// ages ride as `data-ts` attributes and tick in the document's own script, so a
// rewrite is owed only when the board's structure or a session's state changes —
// never for a clock hand.

import { mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { basename, dirname, join } from "node:path";
import {
  EMPTY_BOARD_NOTE,
  isBadOutcome,
  relativeTime,
  rowGlyph,
  scopeSummary,
  shortPaneKey,
  shortTask,
  summaryLine,
  toEpochMs
} from "./render.mjs";

/** The panel entry `orca-plugin.json` declares; the worker rewrites that file. */
export const PANEL_ENTRY = "panel.html";

/** Marker that separates this worker's live output from the committed placeholder. */
export const PANEL_GENERATED_MARKER = "onlyne-sessions panel snapshot";

/** The committed placeholder carries its own marker, so a checkout is healed. */
export const PANEL_PLACEHOLDER_MARKER = "onlyne-sessions panel placeholder";

/** Installed trees are content-addressed: `<plugins>/<key>/<sha256>/`. */
export const PACKAGED_ROOT_RE = /^[0-9a-f]{64}$/i;

/** Past this many times the 5 s cadence, the document marks itself stale. */
export const STALE_AFTER_MS = 15000;

/** The embedded snapshot block's DOM id, shared with the debug command. */
export const JSON_BLOCK_ID = "board-snapshot";

const escapeHtml = (value) =>
  String(value ?? "").replace(
    /[&<>"']/g,
    (character) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[character]
  );

/** JSON safe inside a `<script>` block: `<` cannot open a tag from there. */
const embedJson = (value) => JSON.stringify(value).replace(/</g, "\\u003c");

/**
 * Where the snapshot may be written, or `null` for a tree this worker must
 * leave alone (a content-addressed install) or a root that cannot be addressed.
 */
export function panelWriteTarget({ rootDir, entry = PANEL_ENTRY } = {}) {
  if (typeof rootDir !== "string" || !rootDir) return null;
  if (PACKAGED_ROOT_RE.test(basename(rootDir))) return null;
  if (!entry || entry.includes("/") || entry.includes("\\") || entry.startsWith(".")) return null;
  return join(rootDir, entry);
}

/**
 * What the document depends on: which rows exist where, liveness, a session's
 * lifecycle/agent/delivery/outcome, and which axis is down. Timestamps are
 * deliberately absent — the panel ticks ages itself, and including them would
 * rewrite (and therefore remount) the panel on every scan.
 */
export function panelFingerprint(board) {
  if (!board) return "empty";
  const rows = [...board.rows]
    .map((row) =>
      [
        row.kind,
        row.root ?? "?",
        row.role ?? "?",
        row.taskId ?? "?",
        row.paneKey ?? "?",
        row.live ? 1 : 0,
        row.session?.lifecycle ?? "-",
        row.session?.agent ?? "-",
        row.session?.delivery ?? "-",
        row.session?.outcome ?? "-"
      ].join("|")
    )
    .sort();
  const errors = [...board.errors]
    .map((error) => `${error.scope}|${error.axis}|${error.code}|${error.message}`)
    .sort();
  // `scope` belongs in the fingerprint even though it is not a row: the note
  // below is rendered from it, so a tab axis that changes what it is scoped to
  // still changes what the panel must show.
  return JSON.stringify([board.summary, board.scope ?? null, errors, rows]);
}

/**
 * The snapshot payload. The panel document embeds exactly this object and
 * `onlyne-sessions.debug-board` writes exactly this object, so a JSON dump and
 * what the panel shows can never disagree.
 *
 * `claims` is the one key the board no longer fills. It used to carry the pane
 * claims (`<workspace>/.onlyne/cache/pi-panes/`) that scoped the tab axis, and
 * that authority now rides the session axis instead; the key stays because the
 * committed `panel.html` is generated byte-for-byte from the placeholder render
 * of this function, and the working copy of that file belongs to a dev install.
 */
export function boardPayload(board, { generatedAt = Date.now(), panel = null } = {}) {
  return {
    schema: "onlyne-sessions/board@1",
    generatedAt,
    panel,
    summary: board?.summary ?? null,
    scope: board?.scope ?? null,
    claims: board?.claims ?? null,
    errors: board?.errors ?? [],
    roots: board?.roots ?? [],
    strayTabs: board?.strayTabs ?? [],
    rows: board?.rows ?? []
  };
}

function glyphClass(row) {
  const glyph = rowGlyph(row);
  if (glyph === "●") return "live";
  if (glyph === "✕") return "bad";
  return "dead";
}

/** `<span class="age">`: escaped text plus the stamp the document ticks. */
function ageCell(value) {
  const ms = toEpochMs(value);
  const known = Number.isFinite(ms) && ms > 0;
  const stamp = known ? ` data-ago data-ts="${ms}"` : "";
  return `<span class="age"${stamp}>${escapeHtml(known ? relativeTime(ms) : "—")}</span>`;
}

function sessionState(row) {
  if (!row.session) return "unknown";
  const parts = [row.session.lifecycle, row.session.agent].filter(Boolean);
  if (row.session.outcome) parts.push(row.session.outcome);
  return parts.length ? parts.join("/") : "unknown";
}

function taskTable(group) {
  const rows = group.rows
    .map(
      (row) =>
        `<tr>` +
        `<td class="glyph ${glyphClass(row)}">${rowGlyph(row)}</td>` +
        `<td class="mono">${escapeHtml(shortTask(row.taskId))}</td>` +
        `<td>${escapeHtml(sessionState(row))}</td>` +
        `<td class="age-cell">${ageCell(row.lastOutputAt ?? row.session?.updatedAt)}</td>` +
        `<td class="mono dim">${escapeHtml(row.joined ? shortPaneKey(row.paneKey) : "无 tab")}</td>` +
        `</tr>`
    )
    .join("");
  return `<table><tbody>${rows}</tbody></table>`;
}

function roleSection(group) {
  const presence = group.presence?.presence ?? "no role row";
  const head = `${presence} · ${group.rows.length} tasks · ${group.live} live`;
  return (
    `<div class="group">` +
    `<div class="group-head"><span class="role">${escapeHtml(group.role)}</span>` +
    `<span class="dim">${escapeHtml(head)}</span></div>` +
    taskTable(group) +
    `</div>`
  );
}

function rootSection(root) {
  const failures = root.failures
    .map(
      (failure) =>
        `<p class="error">! ${escapeHtml(failure.axis)}: ${escapeHtml(failure.code)} — ` +
        `${escapeHtml(failure.message)}</p>`
    )
    .join("");
  const groups = root.groups.map(roleSection).join("");
  const head =
    `${root.summary.roles} roles · ${root.summary.sessions} sessions · ` +
    `${root.summary.sessionsWorking} working`;
  return (
    `<h2>${escapeHtml(root.root)}</h2>` +
    `<p class="dim">${escapeHtml(head)}</p>` +
    failures +
    (groups || `<p class="dim">这个 root 还没有 role 行。</p>`)
  );
}

function straySection(tabs) {
  if (!tabs.length) return "";
  const rows = tabs
    .map(
      (row) =>
        `<tr>` +
        `<td class="glyph ${glyphClass(row)}">${rowGlyph(row)}</td>` +
        `<td>${escapeHtml(row.title?.trim() || "(无标题)")}</td>` +
        `<td class="age-cell">${ageCell(row.lastOutputAt)}</td>` +
        `<td class="mono dim">${escapeHtml(shortPaneKey(row.paneKey))}</td>` +
        `<td class="mono dim">wt ${escapeHtml(row.worktreeId ? row.worktreeId.slice(0, 8) : "—")}</td>` +
        `</tr>`
    )
    .join("");
  return `<h2>未 join 的 tab (${tabs.length})</h2><table><tbody>${rows}</tbody></table>`;
}

/**
 * The document's own script: age ticking, and nothing else. No fetch, no
 * postMessage, no storage — the frame stays a document, and the watchdog's ping
 * responder lives in the host shell, not here.
 *
 * An age is `now - data-ts`, exact however long the snapshot has sat on disk;
 * only the styling changes past the cadence, which is the signal that no new
 * scan has arrived.
 */
export const PANEL_SCRIPT = `(function () {
  'use strict'
  var STALE_AFTER_MS = ${STALE_AFTER_MS}
  var nodes = null
  function format(ms) {
    if (!isFinite(ms) || ms < 0) return '—'
    var seconds = Math.floor(ms / 1000)
    if (seconds < 60) return seconds + 's'
    var minutes = Math.floor(seconds / 60)
    if (minutes < 60) return minutes + 'm'
    var hours = Math.floor(minutes / 60)
    if (hours < 24) return hours + 'h'
    return Math.floor(hours / 24) + 'd'
  }
  function tick() {
    if (!nodes) nodes = Array.prototype.slice.call(document.querySelectorAll('[data-ago]'))
    var stamp = document.getElementById('stamp')
    var generatedAt = stamp ? Number(stamp.getAttribute('data-ts')) : NaN
    var now = Date.now()
    var stale = isFinite(generatedAt) && now - generatedAt > STALE_AFTER_MS
    for (var i = 0; i < nodes.length; i++) {
      var node = nodes[i]
      var text = format(now - Number(node.getAttribute('data-ts')))
      if (node.textContent !== text) node.textContent = text
    }
    if (stamp && stale) document.body.className = 'stale'
  }
  tick()
  setInterval(tick, 1000)
})()`;

const STYLE = `
  :root { color-scheme: light dark; }
  body {
    margin: 0;
    padding: 12px 14px 22px;
    background: var(--background, #1b1b1f);
    color: var(--foreground, #e6e6e9);
    font: 12px/1.55 ui-sans-serif, -apple-system, "SF Pro Text", "PingFang SC", sans-serif;
  }
  h1 { margin: 0 0 2px; font-size: 14px; font-weight: 600; }
  h2 {
    margin: 16px 0 6px; font-size: 11px; font-weight: 600; color: var(--muted-foreground, #9a9aa2);
    letter-spacing: 0.04em; word-break: break-all;
  }
  p { margin: 0 0 6px; }
  .dim { color: var(--muted-foreground, #9a9aa2); }
  .mono { font-family: ui-monospace, "SF Mono", Menlo, monospace; font-size: 11px; }
  .card {
    margin: 10px 0; padding: 9px 11px;
    border: 1px solid var(--border, #33333a); border-radius: var(--radius, 8px);
    background: var(--card, #232329);
  }
  .counts { font-weight: 600; }
  .legend { display: grid; grid-template-columns: 1.2em auto; gap: 1px 6px; margin-top: 6px; }
  table { width: 100%; border-collapse: collapse; }
  td { padding: 2px 6px 2px 0; vertical-align: top; }
  td.glyph { width: 1.1em; }
  td.age-cell { text-align: right; white-space: nowrap; }
  .group { margin: 0 0 10px; }
  .group-head { display: flex; gap: 8px; align-items: baseline; margin: 6px 0 2px; }
  .role { font-weight: 600; }
  .live { color: #3fb950; }
  .bad { color: #f85149; }
  .dead { color: var(--muted-foreground, #9a9aa2); }
  .error { color: #f85149; word-break: break-word; }
  body.stale .age { color: var(--muted-foreground, #9a9aa2); }
  .stamp { margin-top: 14px; }
`;

/**
 * Render the panel document for one board, or the committed placeholder.
 *
 * Everything interpolated from a CLI answer — titles, roles, paths, error text —
 * is escaped: those strings come from other processes and are not markup.
 */
export function renderPanelDocument(
  board,
  { generatedAt = Date.now(), panel = null, placeholder = false } = {}
) {
  const marker = placeholder ? PANEL_PLACEHOLDER_MARKER : PANEL_GENERATED_MARKER;
  const stamp = `<span id="stamp" class="age" data-ago data-ts="${generatedAt}">${escapeHtml(
    relativeTime(generatedAt, generatedAt)
  )}</span>`;
  const payload = boardPayload(board, { generatedAt, panel });
  // A board that was read is a snapshot, even when it lists nothing: an empty
  // tab axis (no connected pi at all) is the answer, not "no snapshot yet".
  const hasRows = !placeholder && Boolean(board);
  const body = hasRows
    ? `<p class="dim">文档生成于 ${stamp} 前 · 看板结构或会话状态变化时 worker 重写它（dev 安装即时生效）</p>` +
      `<div class="card"><div class="counts">${escapeHtml(summaryLine(board))}</div>` +
      `<div class="dim">${escapeHtml(scopeSummary(board.scope))}</div>` +
      `<div class="dim">tab 轴 = 一次 <code>orca terminal list --json</code>；session 轴 = 每个配置的 ` +
      `server root 的 <code>onlyne --server-root &lt;S&gt; sessions|roles --json</code>。</div>` +
      `<div class="legend"><span class="live">●</span><span>活 tab（<code>connected=true</code>）</span>` +
      `<span class="dead">○</span><span>tab 轴没列出，或没连上</span>` +
      `<span class="bad">✕</span><span>会话终止不干净（outcome=fault/cancelled）</span></div></div>` +
      board.roots.map(rootSection).join("") +
      straySection(board.strayTabs) +
      (board.errors.length
        ? `<h2>轴级错误 (${board.errors.length})</h2>` +
          board.errors
            .map(
              (error) =>
                `<p class="error">! ${escapeHtml(error.scope)} ${escapeHtml(error.axis)}: ` +
                `${escapeHtml(error.code)} — ${escapeHtml(error.message)}</p>`
            )
            .join("")
        : "")
    : `<div class="card"><p>${escapeHtml(EMPTY_BOARD_NOTE)}</p>` +
      `<p class="dim">这份文档还没有快照：dev 安装时 worker 每次结构变化都会重写它；` +
      `正式安装的树是内容寻址且每次刷新都校验哈希，worker 不写那里。</p></div>`;
  return (
    `<!doctype html>\n<html>\n<head>\n<meta charset="utf-8" />\n` +
    `<title>Onlyne Sessions</title>\n<style>${STYLE}</style>\n</head>\n<body>\n` +
    `<!-- ${marker} -->\n` +
    `<h1>Onlyne Sessions</h1>\n<p class="dim">只读 supervisor 看板 · pluginApi v1（EXPERIMENTAL）</p>\n` +
    `${body}\n<p class="dim stamp">join 只看标题 <code>onlyne:&lt;task_id&gt;</code>` +
    `（弱信号，权威在 adapter/pi 插件协议）</p>\n` +
    `<script type="application/json" id="${JSON_BLOCK_ID}">${embedJson(payload)}</script>\n` +
    `<script>${PANEL_SCRIPT}</script>\n</body>\n</html>\n`
  );
}

const defaultFs = { mkdirSync, readFileSync, renameSync, writeFileSync };

/**
 * Writes the snapshot into the panel entry, and only there.
 *
 * Writes are gated three ways: the tree must be mutable (not content-addressed),
 * the board's fingerprint must have changed, and the file on disk must already
 * carry this worker's live marker. The committed placeholder carries a different
 * marker, so a `git checkout` of it is healed on the next scan instead of
 * waiting for the board to change.
 */
export function createPanelPublisher({
  rootDir,
  entry = PANEL_ENTRY,
  log = () => {},
  fs = defaultFs,
  now = () => Date.now()
} = {}) {
  const target = panelWriteTarget({ rootDir, entry });
  let fingerprint = null;

  function publish(board) {
    if (!target) {
      return { written: false, reason: "installed-tree", path: null };
    }
    const next = panelFingerprint(board);
    let reason = "changed";
    if (next === fingerprint) {
      let current = null;
      try {
        current = fs.readFileSync(target, "utf8");
      } catch {
        current = null;
      }
      if (typeof current === "string" && current.includes(PANEL_GENERATED_MARKER)) {
        return { written: false, reason: "unchanged", path: target };
      }
      reason = "placeholder";
    }
    // The payload names the file it was written to, so the document and the
    // `debug-board` dump describe the same board the same way.
    const document = renderPanelDocument(board, { generatedAt: now(), panel: { target } });
    const temp = `${target}.${process.pid}.tmp`;
    try {
      fs.mkdirSync(dirname(target), { recursive: true });
      fs.writeFileSync(temp, document, "utf8");
      // Atomic: Orca may read the entry at any moment, and half a document is a
      // broken panel.
      fs.renameSync(temp, target);
    } catch (error) {
      log(`panel document not written (${reason}): ${error?.message ?? error}`);
      return { written: false, reason: "write-failed", path: target };
    }
    fingerprint = next;
    return { written: true, reason, path: target, bytes: Buffer.byteLength(document, "utf8") };
  }

  return { publish, target, fingerprint: () => fingerprint };
}
