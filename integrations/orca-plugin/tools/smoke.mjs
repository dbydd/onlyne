// Read-only smoke run against the live Orca desktop.
//
//   node tools/smoke.mjs [--json]
//
// It never creates/closes/switches/renames a tab, never sends terminal text and
// never posts a notification: the guarded runner below fails the run if the
// plugin ever asks for a mutating verb.
//
// What it proves: the real `orca` CLI answers, discovery over the machine's real
// worktree list is clean (no role workspace yet is the expected result), and the
// join renders real live tabs when a mapping file exists — the last one by
// feeding synthetic mapping rows for one real worktree, so no file is written
// anywhere near onlyne's state.

import { existsSync } from "node:fs";
import { execFile } from "node:child_process";
import { createRunner, resolveBinaries } from "../src/runner.mjs";
import { createOrcaCli, paneKeyOf, worktreeSelector } from "../src/orca-cli.mjs";
import { clientSocketPath, createOnlyneCli } from "../src/onlyne-cli.mjs";
import { collectBoard } from "../src/discover.mjs";
import { formatBoard } from "../src/render.mjs";
import { readMapping } from "../src/mapping.mjs";

const MUTATING = /terminal (switch|create|close|rename|send|kill)|worktree (create|rm|remove|delete)/;
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

  section("orca status");
  const status = await orca.status();
  report.steps.status = status.ok
    ? { ok: true, app: status.value?.result?.app?.running ?? status.value?.app?.running, version: status.value?.result?.runtime?.appVersion ?? status.value?.runtime?.appVersion }
    : status;
  write(JSON.stringify(report.steps.status));

  section("orca worktree list");
  const worktrees = await orca.listWorktrees();
  if (!worktrees.ok) {
    write(`FAILED ${worktrees.code}: ${worktrees.message}`);
    return { ...report, forbidden };
  }
  const unique = new Set(worktrees.rows.map((row) => row.path));
  write(`rows=${worktrees.rows.length} unique paths=${unique.size}`);
  for (const row of worktrees.rows.slice(0, 4)) {
    write(`  ${row.path}  (${row.displayName ?? "?"})`);
  }
  report.steps.worktrees = { rows: worktrees.rows.length, uniquePaths: unique.size };

  section("discovery over the real machine");
  const board = await collectBoard({ orca, onlyne, readMapping });
  report.steps.discovery = { workspaces: board.workspaces.length, rows: board.rows.length, summary: board.summary };
  write(`role workspaces=${board.workspaces.length} rows=${board.rows.length}`);
  write(formatBoard(board));
  for (const error of board.errors.slice(0, 5)) write(`  note: ${error.scope} ${error.code} ${error.message}`);

  section("join demo: real tabs + synthetic mapping rows (no file written)");
  const allTerminals = await orca.listTerminals(null);
  const byPath = new Map();
  for (const row of allTerminals.ok ? allTerminals.rows : []) {
    if (!row.worktreePath) continue;
    const bucket = byPath.get(row.worktreePath) ?? [];
    bucket.push(row);
    byPath.set(row.worktreePath, bucket);
  }
  const targetPath = worktrees.rows.map((row) => row.path).find((path) => byPath.has(path));
  const target = worktrees.rows.find((row) => row.path === targetPath);
  if (!target) {
    write("no worktree currently owns a live terminal");
  } else {
    const terminals = await orca.listTerminals(worktreeSelector(target.path));
    if (!terminals.ok) {
      write(`terminal list --worktree failed: ${terminals.code} ${terminals.message}`);
    } else {
      const first = terminals.rows[0];
      const rows = [
        {
          paneKey: paneKeyOf(first) ?? first.handle,
          handle: first.handle,
          taskId: "smoke-task-live",
          sessionId: "smoke-session-live",
          role: "smoke-role",
          worktreeSelector: worktreeSelector(target.path),
          title: "onlyne:smoke-task-live",
          state: "spawned",
          updatedAt: new Date().toISOString(),
          closed: false,
        },
        {
          paneKey: "00000000-0000-4000-8000-000000000000:11111111-1111-4111-8111-111111111111",
          handle: "term_00000000-0000-4000-8000-000000000000",
          taskId: "smoke-task-gone",
          sessionId: "smoke-session-gone",
          role: "smoke-role",
          worktreeSelector: worktreeSelector(target.path),
          title: "onlyne:smoke-task-gone",
          state: "spawned",
          updatedAt: new Date().toISOString(),
          closed: false,
        },
      ];
      const missing = { ok: false, missing: true, path: "(synthetic)", rows: [], tombstones: [], malformed: 0 };
      const demo = await collectBoard({
        orca,
        onlyne,
        readMapping: (path) =>
          path === target.path
            ? { ok: true, missing: false, path: "(synthetic)", rows, tombstones: [], malformed: 0 }
            : missing,
      });
      report.steps.join = {
        path: target.path,
        terminals: terminals.rows.length,
        rows: demo.rows.length,
        live: demo.summary.liveTabs,
      };
      write(`real terminals for ${target.path}: ${terminals.rows.length}; joined rows=${demo.rows.length} live=${demo.summary.liveTabs}`);
      write(formatBoard(demo));
    }
  }

  section("onlyne session probe (degrade path)");
  const socket = clientSocketPath(target?.path ?? "/tmp");
  write(`client socket ${socket} exists=${existsSync(socket)}`);
  const probe = await onlyne.querySessions(socket);
  report.steps.onlyne = probe.ok
    ? { ok: true, sessions: probe.sessions.length }
    : { ok: false, code: probe.code, message: probe.message };
  write(JSON.stringify(report.steps.onlyne));
  const version = await runner.run(binaries.onlyneBin, ["--version"]);
  write(`onlyne --version -> ${version.ok ? version.stdout.trim() : `${version.code}: ${version.message}`}`);

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
