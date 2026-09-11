// Read-only smoke run against the live Orca desktop.
//
//   node tools/smoke.mjs [--json]
//
// It never creates/closes/switches/renames a tab, never sends terminal text and
// never posts a notification: the guarded runner below fails the run if the
// plugin ever asks for a mutating verb.
//
// What it proves: the real `orca` CLI answers, the flat tab list is clean (no
// worktree is scanned individually any more), every configured server root
// answers or degrades with its own code, the join renders real live tabs, and
// the panel document this worker would publish is generated and validated —
// into a temp root, so the run never touches the plugin tree it is testing.

import { mkdtempSync, readFileSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { execFile } from "node:child_process";
import { createRunner, resolveBinaries } from "../src/runner.mjs";
import { createOrcaCli, paneKeyOf } from "../src/orca-cli.mjs";
import { createOnlyneCli, normalizeRoleRow, normalizeSessionRow } from "../src/onlyne-cli.mjs";
import { collectBoard, taskIdFromTitle } from "../src/board.mjs";
import {
  JSON_BLOCK_ID,
  createPanelPublisher,
  panelFingerprint,
  panelWriteTarget
} from "../src/panel-document.mjs";
import { formatBoard } from "../src/render.mjs";

const MUTATING = /terminal (switch|create|close|rename|send|kill)|worktree (create|rm|remove|delete)/;
const SOCKET_RELATIVE = join(".onlyne", "run", "s");
const forbidden = [];

function execFileAsync(binary, args, options) {
  const line = [binary, ...args].join(" ");
  if (MUTATING.test(line)) forbidden.push(line);
  return new Promise((resolve, reject) => {
    execFile(binary, args, options, (error, stdout, stderr) => {
      if (error) {
        error.stdout = stdout;
        error.stderr = stderr;
        reject(error);
        return;
      }
      resolve({ stdout, stderr });
    });
  });
}

function section(title) {
  process.stdout.write(`\n=== ${title} ===\n`);
}

export async function runSmoke({ write = (line) => process.stdout.write(`${line}\n`) } = {}) {
  const binaries = resolveBinaries();
  const runner = createRunner({ exec: execFileAsync });
  const orca = createOrcaCli({ runner, binary: binaries.orcaBin });
  const onlyne = createOnlyneCli({ runner, binary: binaries.onlyneBin });
  const report = { binaries, steps: {} };

  section("binaries");
  write(`orca   : ${binaries.orcaBin}`);
  write(`onlyne : ${binaries.onlyneBin}${binaries.configLoaded ? ` (config ${binaries.configPath})` : " (zero-config)"}`);
  write(`serverRoots: ${binaries.serverRoots.length ? binaries.serverRoots.join(", ") : "(none — tab axis only)"}`);

  section("orca status");
  const status = await orca.status();
  report.steps.status = status.ok
    ? {
        ok: true,
        app: status.value?.result?.app?.running ?? status.value?.app?.running,
        version: status.value?.result?.runtime?.appVersion ?? status.value?.runtime?.appVersion,
      }
    : status;
  write(JSON.stringify(report.steps.status));

  section("orca terminal list (flat: every worktree, one call)");
  const tabs = await orca.listTerminals();
  if (!tabs.ok) {
    write(`FAILED ${tabs.code}: ${tabs.message}`);
    return { ...report, forbidden };
  }
  const worktrees = new Set(tabs.rows.map((row) => row.worktreeId ?? "(none)"));
  write(`rows=${tabs.rows.length} worktrees=${worktrees.size}`);
  for (const row of tabs.rows.slice(0, 5)) {
    write(`  ${paneKeyOf(row) ?? "(no pane)"}  ${row.connected ? "live" : "down"}  ${row.title ?? ""}`);
  }
  report.steps.tabs = { rows: tabs.rows.length, worktrees: worktrees.size };

  section("board over the configured server roots");
  const board = await collectBoard({ orca, onlyne, serverRoots: binaries.serverRoots });
  report.steps.board = {
    ok: board.ok,
    roots: board.summary.roots,
    rootsFailed: board.summary.rootsFailed,
    tabs: board.summary.tabs,
    joined: board.summary.joined,
    strayTabs: board.summary.strayTabs,
    sessions: board.summary.sessions,
    errors: board.errors,
  };
  write(
    `roots=${board.summary.roots} (failed ${board.summary.rootsFailed}) tabs=${board.summary.tabs} ` +
      `joined=${board.summary.joined} stray=${board.summary.strayTabs} sessions=${board.summary.sessions}`
  );
  write(formatBoard(board));
  if (!binaries.serverRoots.length) {
    write("no serverRoots configured; add them to ~/.config/onlyne-sessions/config.json:");
    write('  { "serverRoots": ["/abs/path/to/server-root"] }');
  }

  section("join demo: real tab titles, synthetic sessions (nothing written)");
  const titled = tabs.rows.filter((row) => taskIdFromTitle(row.title));
  const syntheticRoot = "(synthetic) smoke-root";
  if (!titled.length) {
    write("no live tab carries a title of the form onlyne:<task_id>; the join has nothing to annotate");
    const demo = await collectBoard({
      orca,
      onlyne: { querySessions: async () => ({ ok: true, sessions: [] }), queryRoles: async () => ({ ok: true, roles: [] }) },
      serverRoots: [syntheticRoot],
    });
    write(formatBoard(demo));
    report.steps.join = { titled: 0, strayTabs: demo.summary.strayTabs };
  } else {
    // One synthetic session per titled tab, each reporting the pane of the real
    // tab it stands in — the axis is scoped by that report, so a stub without
    // one would scope its own tabs away and demo nothing.
    const demoTabs = new Map();
    for (const row of titled) {
      const taskId = taskIdFromTitle(row.title);
      if (!demoTabs.has(taskId)) demoTabs.set(taskId, row);
    }
    const taskIds = [...demoTabs.keys()].slice(0, 3);
    const stub = {
      querySessions: async () => ({
        ok: true,
        sessions: taskIds.map((taskId) =>
          normalizeSessionRow({
            task_id: taskId,
            role: "smoke",
            session_id: `smoke-${taskId}`,
            public_lifecycle: "working",
            projection: {
              lifecycle: "working",
              agent: "running",
              observed: { host: { orca: { pane_key: paneKeyOf(demoTabs.get(taskId)) } } },
            },
            updated_at: new Date().toISOString(),
          })
        ),
      }),
      queryRoles: async () => ({
        ok: true,
        roles: [normalizeRoleRow({ name: "smoke", state: "online", sessions: taskIds.length })],
      }),
    };
    const demo = await collectBoard({ orca, onlyne: stub, serverRoots: [syntheticRoot] });
    report.steps.join = {
      titled: titled.length,
      taskIds,
      joined: demo.summary.joined,
      strayTabs: demo.summary.strayTabs,
    };
    write(`titled tabs=${titled.length} task ids=${taskIds.join(", ")}`);
    write(formatBoard(demo));
  }

  section("onlyne admin probe (per configured root)");
  report.steps.roots = [];
  if (!binaries.serverRoots.length) {
    write("no configured server root to probe");
  }
  for (const root of binaries.serverRoots) {
    const socket = join(root, SOCKET_RELATIVE);
    const sessions = await onlyne.querySessions(root);
    const roles = await onlyne.queryRoles(root);
    const row = {
      root,
      socketExists: existsSync(socket),
      sessions: sessions.ok ? { ok: true, rows: sessions.sessions.length } : { ok: false, code: sessions.code, message: sessions.message },
      roles: roles.ok ? { ok: true, rows: roles.roles.length } : { ok: false, code: roles.code, message: roles.message },
    };
    report.steps.roots.push(row);
    write(`${root}  socket=${row.socketExists}  sessions=${JSON.stringify(row.sessions)}  roles=${JSON.stringify(row.roles)}`);
  }
  const version = await runner.run(binaries.onlyneBin, ["--version"]);
  write(`onlyne --version -> ${version.ok ? version.stdout.trim() : `${version.code}: ${version.message}`}`);
  section("panel document (generated into a temp root, never the plugin tree)");
  {
    const pluginRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
    const liveTarget = panelWriteTarget({ rootDir: pluginRoot });
    write(
      liveTarget
        ? `this tree is mutable → the worker rewrites ${liveTarget} (the panel reloads on the dev watcher)`
        : "content-addressed install → the worker never writes there; the panel keeps its installed document"
    );
    report.steps.panelTarget = liveTarget ? { kind: "dev-tree", path: liveTarget } : { kind: "install" };
    const panelRoot = mkdtempSync(join(tmpdir(), "onlyne-panel-smoke-"));
    const publisher = createPanelPublisher({ rootDir: panelRoot });
    const first = publisher.publish(board);
    const second = publisher.publish(board);
    const document = readFileSync(join(panelRoot, "panel.html"), "utf8");
    const payload = JSON.parse(
      document.match(new RegExp(`id="${JSON_BLOCK_ID}">([\\s\\S]*?)</script>`))[1].replace(/\\u003c/g, "<")
    );
    report.steps.panel = {
      written: first.written,
      bytes: first.bytes,
      secondWrite: second.written,
      fingerprint: panelFingerprint(board).length,
      payloadRows: payload.rows.length,
    };
    write(
      `wrote=${first.written} bytes=${first.bytes} rewrite-on-unchanged=${second.written} ` +
        `size-on-disk=${statSync(join(panelRoot, "panel.html")).size} ` +
        `rows-in-embedded-snapshot=${payload.rows.length}`
    );
  }

  section("mutation guard");
  write(forbidden.length ? `VIOLATIONS: ${forbidden.join(" | ")}` : "no mutating orca verb was invoked");
  report.forbidden = forbidden;
  return report;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const report = await runSmoke();
  if (process.argv.includes("--json")) process.stdout.write(`\n${JSON.stringify(report, null, 2)}\n`);
  process.exit(report.forbidden?.length ? 1 : 0);
}
