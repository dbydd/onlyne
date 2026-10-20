#!/usr/bin/env node
/**
 * omp-live — real OMP acceptance for `v2.11 Lifecycle is declared, not inferred`.
 *
 * What this runs, in one resident host:
 *
 *   1. an `omp` process started in a *body* directory (`--cwd`) whose
 *      `.pi/onlyne.json` names a role, a resident session id and the role
 *      workspace that serves its socket — the deployed shape, where the agent
 *      body does not live inside the cluster tree;
 *   2. the plugin from *this checkout* loaded through the host's own extension
 *      loader (`--no-extensions -e <checkout>/plugins/onlyne-agent-pi/src`), so
 *      ambient installs on the machine contribute nothing;
 *   3. a real `task` subagent spawned by the parent's own `task` tool, with its
 *      real disposal (the plugin must not open a second connection for the
 *      parent's resident alias while the child exists);
 *   4. a note delivered by the real `onlyne` CLI into the still-mounted parent;
 *   5. a top-level session replacement (`/new` = RPC `new_session`) with the
 *      mount following the replacement rather than re-mounting;
 *   6. a real parent teardown, after which the server's session row is `exited`.
 *
 * Every step records the evidence it read and fails loudly when a step produced
 * none. The evidence directory keeps the raw RPC frame stream and the host's
 * stderr whether the run passes or fails.
 *
 * Usage (run.py owns the topology; this file owns driving the host):
 *
 *   # one flag: run.py's own export carries tree/clusters/workspaces/resident_session
 *   bun examples/lifecycle/omp-live.mjs --paths-json <tree>/paths.json
 *
 *   # or spelled out
 *   bun examples/lifecycle/omp-live.mjs \
 *     --server-root <central> --workspace <central>/<role> \
 *     --cwd <body> --role <role> --session-id <role>-main \
 *     --runtime-dir <tree>/runtime [--evidence-dir <dir>] [...]
 *
 * See `--help` for every option. No platform traffic, no credential, no
 * installed-package change: the only sockets spoken to are the example's own
 * runtime directory, which `--runtime-dir` names.
 */

import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "..", "..");
const HOME = process.env.HOME ?? "";

/** Non-zero exit for a failed assertion; the report says which one. */
const EXIT_FAILED = 1;
/** Non-zero exit for a usage error, matching the repo's other scripts. */
const EXIT_USAGE = 2;

const USAGE = `omp-live — real OMP resident-parent acceptance (v2.11 lifecycle fix)

Topology (run.py owns these paths; every one may also come from --paths-json)
  --server-root <dir>     central server root (the onlyne --server-root)
  --workspace <dir>       the role workspace whose client serves the parent socket
  --cwd <dir>             OMP body root; .pi/onlyne.json lives here
  --role <name>           parent role (default: read back from .pi/onlyne.json)
  --session-id <id>       resident session id (default: read back from .pi/onlyne.json)
  --runtime-dir <dir>     ONLYNE_RUNTIME_DIR for the host and every CLI call (required;
                          never the machine-level directory, which belongs to the operator)
  --session-dir <dir>     OMP session files (default: <cwd>/.pi/sessions)
  --evidence-dir <dir>    report + captured streams (default: <cwd>/.pi/omp-live)
  --paths-json <file|->   JSON object of the options above; explicit flags win

Binaries
  --omp-bin <path>        omp entry (default: $OMP_BIN, else ~/.bun/bin/omp)
  --onlyne-bin <path>     onlyne CLI (default: <repo>/target/debug/onlyne)
  --plugin-path <dir>     this checkout's plugin (default: <repo>/plugins/onlyne-agent-pi)

Scenario
  --marker <text>         unique marker (default: omp-live-<pid>-<time>)
  --note-from <role>      note sender role; default: derived from 'onlyne roles'
  --task-agent <name>     child agent name; default: derived from the task tool roster
  --env KEY=VALUE         repeatable extra environment for the host and CLI calls
  --ready-timeout-s <n>   wait for the host's ready frame (default 60)
  --mount-timeout-s <n>   wait for the plugin's mount line (default 90)
  --turn-timeout-s <n>    wait for one model turn (default 300)
  --note-timeout-s <n>    wait for a note to land in the host (default 120)
  --exit-timeout-s <n>    wait for the process to leave and the row to read exited (default 30)
  --task-isolated         ask for an isolated child (default: in-process, matching the fix)
  --inert-pattern <regex> plugin log pattern that names the child as inert (default: "staying inert")
  --no-require-inert-log  do not fail when that line is absent (behaviour is still asserted)
  --no-auto-approve       do not pass --auto-approve to the host (default: pass it)
  --smoke                 wiring check only: preflight, ready, mount, teardown (no model turn)
  --keep-going            keep stepping after a failed assertion
  --no-write-config       refuse to write a missing .pi/onlyne.json
`;

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

function fail(code, message) {
  process.stderr.write(`${message}\n`);
  process.exit(code);
}

function parseArgv(argv) {
  const flags = new Map();
  const env = [];
  for (let index = 0; index < argv.length; index++) {
    const token = argv[index];
    if (!token.startsWith("--")) fail(EXIT_USAGE, `omp-live: unexpected positional argument ${token}`);
    const eq = token.indexOf("=");
    const name = eq === -1 ? token : token.slice(0, eq);
    let value;
    if (eq !== -1) value = token.slice(eq + 1);
    else if (index + 1 < argv.length && !argv[index + 1].startsWith("--")) value = argv[++index];
    if (name === "--env") {
      if (value === undefined) fail(EXIT_USAGE, "omp-live: --env needs KEY=VALUE");
      env.push(value);
      continue;
    }
    if (value === undefined) flags.set(name, true);
    else flags.set(name, value);
  }
  return { flags, env };
}

function numberOption(flags, name, fallback) {
  const raw = flags.get(name);
  if (raw === undefined) return fallback;
  const value = Number(raw);
  if (!Number.isFinite(value) || value <= 0) fail(EXIT_USAGE, `omp-live: ${name} needs a positive number`);
  return value;
}

function stringOption(flags, name) {
  const raw = flags.get(name);
  if (raw === undefined || raw === true) return undefined;
  return String(raw);
}

function readPathsJson(spec) {
  if (!spec) return {};
  const raw = spec === "-" ? readFileSync(0, "utf8") : readFileSync(spec, "utf8");
  const parsed = JSON.parse(raw);
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    fail(EXIT_USAGE, "omp-live: --paths-json must hold a JSON object");
  }
  return parsed;
}

/** The `.pi/onlyne.json` a resident body carries, in the deployed shape. */
function residentConfigLine({ role, sessionId, workspace }) {
  return `${JSON.stringify({ enabled: true, resident: true, role, sessionId, workspace, watch: { autoStart: true } }, null, 2)}\n`;
}

function resolveOptions(argv) {
  const { flags, env } = parseArgv(argv);
  if (flags.has("--help") || flags.has("-h")) {
    process.stdout.write(USAGE);
    process.exit(0);
  }
  const fromJson = readPathsJson(stringOption(flags, "--paths-json"));
  // run.py exports one document describing the whole tree
  // (`examples/lifecycle/run.py` `describe`): `tree`, `clusters.center.server_root`,
  // `workspaces[role]`, `resident_session{role,workspace,server_root,session_id}`.
  // Flat keys win over the nested ones, and explicit flags win over both.
  const nested = fromJson.resident_session ?? fromJson.residentSession ?? {};
  const tree = typeof fromJson.tree === "string" && fromJson.tree !== "" ? fromJson.tree : undefined;
  const pick = (key, extra = []) => {
    const explicit = stringOption(flags, `--${key}`);
    if (explicit !== undefined) return explicit;
    const camel = key.replace(/-([a-z])/g, (_match, letter) => letter.toUpperCase());
    const snake = key.replace(/-/g, "_");
    const value = [fromJson[key], fromJson[camel], nested[camel], nested[snake], ...extra].find(
      (candidate) => typeof candidate === "string" && candidate !== "",
    );
    return value;
  };

  const role = pick("role") ?? (typeof fromJson.role === "string" ? fromJson.role : undefined);
  // The body root is deliberately not the role workspace: the deployed shape runs
  // the agent body outside the cluster tree (Alexandria vs central/caerulea).
  const cwd =
    pick("cwd", [tree && role ? join(tree, "omp-body", role) : undefined]) ??
    process.cwd();
  if (!isAbsolute(cwd)) fail(EXIT_USAGE, "omp-live: --cwd must be absolute");
  const pluginPath = pick("plugin-path") ?? join(REPO, "plugins", "onlyne-agent-pi");
  const sessionDir = pick("session-dir") ?? join(cwd, ".pi", "sessions");

  const configPath = join(cwd, ".pi", "onlyne.json");
  let config = null;
  if (existsSync(configPath)) {
    try {
      config = JSON.parse(readFileSync(configPath, "utf8"));
    } catch (error) {
      fail(EXIT_USAGE, `omp-live: ${configPath} is not valid JSON (${error.message})`);
    }
  }
  const configRole = typeof config?.role === "string" ? config.role : undefined;
  const configSessionId = typeof config?.sessionId === "string" ? config.sessionId : undefined;
  const effectiveRole = role ?? configRole ?? fail(EXIT_USAGE, "omp-live: --role is required when .pi/onlyne.json names none");
  const sessionId = pick("session-id") ?? configSessionId ?? `${effectiveRole}-main`;
  const workspace =
    pick("workspace", [fromJson.workspaces?.[effectiveRole]]) ??
    (typeof config?.workspace === "string" ? config.workspace : undefined);

  const ompBin = pick("omp-bin") ?? process.env.OMP_BIN ?? (existsSync(join(HOME, ".bun", "bin", "omp")) ? join(HOME, ".bun", "bin", "omp") : "omp");
  const onlyneBin = pick("onlyne-bin") ?? join(REPO, "target", "debug", "onlyne");
  const marker = pick("marker") ?? `omp-live-${process.pid}-${Date.now()}`;
  const serverRoot = pick("server-root", [fromJson.clusters?.center?.server_root]);
  if (!serverRoot) fail(EXIT_USAGE, "omp-live: --server-root is required (the central server root)");
  // Never fall back to the machine-level `/tmp/onlyne-<uid>`: that directory
  // holds the operator's own registrations, and dialling a socket this example
  // did not bind is exactly the cross-talk an isolated run must not risk.
  const runtimeDir =
    pick("runtime-dir", [tree ? join(tree, "runtime") : undefined]) ??
    process.env.ONLYNE_RUNTIME_DIR?.trim() ??
    process.env.LIFECYCLE_RUNTIME_DIR?.trim() ??
    null;
  if (!runtimeDir) {
    fail(EXIT_USAGE, "omp-live: --runtime-dir is required (or export ONLYNE_RUNTIME_DIR); the example must not use the machine-level runtime directory");
  }

  const envEntries = { ...(fromJson.env && typeof fromJson.env === "object" ? fromJson.env : {}) };
  for (const entry of env) {
    const eq = entry.indexOf("=");
    if (eq <= 0) fail(EXIT_USAGE, `omp-live: --env needs KEY=VALUE, got ${entry}`);
    envEntries[entry.slice(0, eq)] = entry.slice(eq + 1);
  }

  return {
    cwd,
    configPath,
    configPresent: config !== null,
    pluginPath,
    sessionDir,
    evidenceDir: pick("evidence-dir") ?? join(cwd, ".pi", "omp-live"),
    serverRoot,
    workspace,
    runtimeDir,
    role: effectiveRole,
    sessionId,
    ompBin,
    onlyneBin,
    marker,
    noteFrom: pick("note-from"),
    taskAgent: pick("task-agent"),
    taskIsolated: flags.has("--task-isolated"),
    inertPattern: pick("inert-pattern") ?? "staying inert",
    requireInertLog: !flags.has("--no-require-inert-log"),
    autoApprove: !flags.has("--no-auto-approve"),
    smoke: flags.has("--smoke"),
    keepGoing: flags.has("--keep-going"),
    writeConfig: !flags.has("--no-write-config"),
    env: envEntries,
    timeouts: {
      ready: numberOption(flags, "--ready-timeout-s", 60) * 1000,
      mount: numberOption(flags, "--mount-timeout-s", 90) * 1000,
      turn: numberOption(flags, "--turn-timeout-s", 300) * 1000,
      note: numberOption(flags, "--note-timeout-s", 120) * 1000,
      exit: numberOption(flags, "--exit-timeout-s", 30) * 1000,
      cli: numberOption(flags, "--cli-timeout-s", 45) * 1000,
    },
    extraFlags: Array.isArray(fromJson.ompArgs) ? fromJson.ompArgs.map(String) : [],
  };
}

// ---------------------------------------------------------------------------
// RPC framing: newline-delimited JSON, protocol v2 reassembles `rpc_chunk`
// ---------------------------------------------------------------------------

class FrameDecoder {
  #pending = null;

  /** One parsed line in, one logical frame out (or undefined while chunks accumulate). */
  push(value) {
    if (!value || typeof value !== "object" || value.type !== "rpc_chunk") {
      if (this.#pending) throw new Error("rpc chunk sequence interrupted");
      return value;
    }
    const { chunkId, index, count, byteLength, data } = value;
    const bytes = Buffer.from(String(data ?? ""), "base64");
    if (!this.#pending) {
      this.#pending = { chunkId, count, byteLength, nextIndex: 0, chunks: [], received: 0 };
    }
    const pending = this.#pending;
    if (pending.chunkId !== chunkId || pending.nextIndex !== index) throw new Error("rpc chunk sequence mismatch");
    pending.chunks.push(bytes);
    pending.received += bytes.byteLength;
    pending.nextIndex += 1;
    if (pending.nextIndex < pending.count) return undefined;
    this.#pending = null;
    if (pending.received !== pending.byteLength) throw new Error("rpc chunk sequence length mismatch");
    return JSON.parse(Buffer.concat(pending.chunks).toString("utf8"));
  }
}

const MOUNT_LINE = /^\[pi-onlyne\] session (\S+) role=(\S+) socket=(\S+)/;

/** Subagent statuses that mean the child's run is over and it was reclaimed. */
const CHILD_TERMINAL_STATUSES = new Set(["completed", "failed", "aborted", "cancelled", "parked", "idle", "error"]);

/** A resident `omp --mode rpc` host, with the frames and stderr it produced. */
class OmpHost {
  constructor(options) {
    this.options = options;
    this.child = null;
    this.frames = [];
    this.stderrLines = [];
    this.exit = null;
    this.decoder = new FrameDecoder();
    this.pending = new Map();
    this.waiters = new Set();
    this.stdoutTail = "";
    this.stderrTail = "";
    this.#sequence = 0;
  }

  #sequence;

  get mountLines() {
    return this.stderrLines.filter((line) => MOUNT_LINE.test(line));
  }

  get pluginErrors() {
    return this.frames.filter(
      (frame) => frame.type === "extension_error" && String(frame.extensionPath ?? "").includes(this.options.pluginRoot),
    );
  }

  start() {
    const args = [
      "--mode", "rpc", "--no-ui",
      "--no-extensions", "-e", this.options.extensionDir,
      "--session-dir", this.options.sessionDir,
      "--cwd", this.options.cwd,
    ];
    if (this.options.autoApprove) args.push("--auto-approve");
    args.push(...this.options.extraFlags);

    this.child = spawn(this.options.ompBin, args, {
      cwd: this.options.cwd,
      env: { ...process.env, ...this.options.env, ...(this.options.runtimeDir ? { ONLYNE_RUNTIME_DIR: this.options.runtimeDir } : {}) },
      stdio: ["pipe", "pipe", "pipe"],
    });
    this.commandLine = `${this.options.ompBin} ${args.join(" ")}`;
    this.child.stdout.on("data", (chunk) => this.#onStdout(chunk));
    this.child.stderr.on("data", (chunk) => this.#onStderr(chunk));
    this.child.on("exit", (code, signal) => {
      this.exit = { code, signal, at: new Date().toISOString() };
      this.#notify();
    });
    return this;
  }

  #onStdout(chunk) {
    this.stdoutTail += chunk.toString("utf8");
    const lines = this.stdoutTail.split("\n");
    this.stdoutTail = lines.pop() ?? "";
    for (const line of lines) {
      if (!line.trim()) continue;
      let frame;
      try {
        frame = this.decoder.push(JSON.parse(line));
      } catch (error) {
        frame = { type: "rpc_frame_unparsed", error: String(error), raw: line.slice(0, 400) };
      }
      if (frame === undefined) continue;
      this.frames.push(frame);
      if (frame.type === "response" && typeof frame.id === "string") {
        const settle = this.pending.get(frame.id);
        if (settle) {
          this.pending.delete(frame.id);
          settle(frame);
          // Still notify: a predicate may be waiting on this frame's arrival.
          this.#notify();
          continue;
        }
      }
      this.#notify();
    }
  }

  #onStderr(chunk) {
    this.stderrTail += chunk.toString("utf8");
    const lines = this.stderrTail.split("\n");
    this.stderrTail = lines.pop() ?? "";
    for (const line of lines) {
      this.stderrLines.push(line);
    }
    this.#notify();
  }

  #notify() {
    for (const waiter of [...this.waiters]) waiter();
  }

  send(frame) {
    if (!this.child) throw new Error("host not started");
    this.child.stdin.write(`${JSON.stringify(frame)}\n`);
  }

  request(type, extra = {}, timeoutMs = 30_000) {
    const id = `omp-live-${++this.#sequence}`;
    const settled = new Promise((resolveFn, rejectFn) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        rejectFn(new Error(`${type} produced no response within ${timeoutMs}ms`));
      }, timeoutMs);
      this.pending.set(id, (frame) => {
        clearTimeout(timer);
        resolveFn(frame);
      });
    });
    this.send({ id, type, ...extra });
    return settled;
  }

  /** Wait until `predicate(host)` is true, checking what has already arrived first. */
  waitFor(label, predicate, timeoutMs) {
    const check = () => {
      try {
        return predicate(this);
      } catch {
        return false;
      }
    };
    if (check()) return Promise.resolve();
    return new Promise((resolveFn, rejectFn) => {
      const timer = setTimeout(() => {
        this.waiters.delete(waiter);
        rejectFn(new Error(`timed out after ${timeoutMs}ms waiting for ${label}`));
      }, timeoutMs);
      const waiter = () => {
        if (!check()) return;
        clearTimeout(timer);
        this.waiters.delete(waiter);
        resolveFn();
      };
      this.waiters.add(waiter);
    });
  }

  kill(signal) {
    if (!this.child || this.exit) return;
    this.child.kill(signal);
  }
}

// ---------------------------------------------------------------------------
// Helpers over the raw evidence
// ---------------------------------------------------------------------------

/** Every string in a value, for marker searches over a frame. */
function stringsIn(value) {
  const out = [];
  const walk = (node) => {
    if (typeof node === "string") out.push(node);
    else if (Array.isArray(node)) for (const item of node) walk(item);
    else if (node && typeof node === "object") for (const key of Object.keys(node)) walk(node[key]);
  };
  walk(value);
  return out;
}

function frameContains(frame, marker) {
  return stringsIn(frame).some((text) => text.includes(marker));
}

/**
 * Where a note showed up: the model-visible messages carrying it. A frame that
 * merely echoes the marker somewhere else (an event payload, a log line) is not
 * the delivery, so only message frames count.
 */
function noteCarriers(host, marker) {
  return host.frames
    .filter((frame) => (frame.type === "message_start" || frame.type === "message_end") && frameContains(frame, marker))
    .map((frame) => ({
      frame: frame.type,
      role: frame.message?.role ?? null,
      text: stringsIn(frame.message?.content)
        .filter((text) => text.includes(marker))
        .join(" ")
        .slice(0, 300),
    }));
}

/** Every session row anywhere in a CLI answer, normalized across admin surfaces. */
function sessionRows(value) {
  const rows = [];
  const walk = (node) => {
    if (Array.isArray(node)) {
      for (const item of node) walk(item);
      return;
    }
    if (!node || typeof node !== "object") return;
    const lifecycle = node.lifecycle ?? node.public_lifecycle ?? node.projection?.lifecycle;
    if (typeof node.session_id === "string" && typeof lifecycle === "string") {
      rows.push({ ...node, lifecycle });
      return;
    }
    for (const key of Object.keys(node)) walk(node[key]);
  };
  walk(value);
  return rows;
}

function runCli(options, args) {
  const run = spawnSync(options.onlyneBin, args, {
    cwd: options.cwd,
    env: { ...process.env, ...options.env, ...(options.runtimeDir ? { ONLYNE_RUNTIME_DIR: options.runtimeDir } : {}) },
    encoding: "utf8",
    timeout: options.timeouts.cli,
  });
  const stdout = run.stdout ?? "";
  let body = null;
  try {
    body = JSON.parse(stdout.trim());
  } catch {
    body = null;
  }
  return {
    argv: [options.onlyneBin, ...args].join(" "),
    code: run.status,
    signal: run.signal ?? null,
    error: run.error ? String(run.error.message ?? run.error) : null,
    stdout: stdout.trim().slice(0, 4000),
    stderr: (run.stderr ?? "").trim().slice(0, 2000),
    body,
  };
}

function readJsonl(path) {
  const entries = [];
  for (const line of readFileSync(path, "utf8").split("\n")) {
    if (!line.trim()) continue;
    try {
      entries.push(JSON.parse(line));
    } catch {
      entries.push({ type: "unparsed", raw: line.slice(0, 200) });
    }
  }
  return entries;
}

/** Newest `.jsonl` files under a session directory, newest mtime first. */
function sessionFiles(dir, limit = 8) {
  if (!existsSync(dir)) return [];
  const found = [];
  const walk = (current, depth) => {
    if (depth > 3) return;
    for (const name of readdirSync(current, { withFileTypes: true })) {
      const path = join(current, name.name);
      if (name.isDirectory()) walk(path, depth + 1);
      else if (name.name.endsWith(".jsonl")) found.push({ path, mtime: statSync(path).mtimeMs });
    }
  };
  walk(dir, 0);
  return found.sort((a, b) => b.mtime - a.mtime).slice(0, limit);
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

const report = {
  tool: "omp-live",
  startedAt: new Date().toISOString(),
  steps: [],
  failures: [],
};
/** Where the report lands once the evidence directory is known. */
let reportPath = null;

function sanitizeOptions(options) {
  return {
    cwd: options.cwd,
    configPath: options.configPath,
    configPresent: options.configPresent,
    pluginPath: options.pluginPath,
    sessionDir: options.sessionDir,
    evidenceDir: options.evidenceDir,
    serverRoot: options.serverRoot ?? null,
    workspace: options.workspace ?? null,
    runtimeDir: options.runtimeDir ?? null,
    role: options.role,
    sessionId: options.sessionId,
    ompBin: options.ompBin,
    onlyneBin: options.onlyneBin,
    marker: options.marker,
    noteFrom: options.noteFrom ?? null,
    taskAgent: options.taskAgent ?? null,
    taskIsolated: options.taskIsolated,
    inertPattern: options.inertPattern,
    requireInertLog: options.requireInertLog,
    autoApprove: options.autoApprove,
    env: Object.fromEntries(Object.entries(options.env).map(([key, value]) => [key, /TOKEN|SECRET|KEY|PASSWORD/i.test(key) ? "<redacted>" : value])),
    timeouts: options.timeouts,
  };
}

class StepFailed extends Error {}

function record(name, evidence, ok, failure) {
  report.steps.push({ name, ok, at: new Date().toISOString(), evidence, failure: failure ?? null });
  if (!ok) {
    report.failures.push({ name, failure });
    process.stderr.write(`FAIL ${name}: ${failure}\n`);
  } else {
    process.stdout.write(`ok   ${name}\n`);
  }
  return ok;
}

function require(condition, message) {
  if (!condition) throw new StepFailed(message);
}

/**
 * Poll a predicate that reads outside the host — a session row the server
 * writes after the process is gone, say. The host's own event loop stops when
 * it exits, so such a wait cannot ride frame or log events.
 */
async function pollUntil(label, predicate, timeoutMs, intervalMs = 2000) {
  const deadline = Date.now() + timeoutMs;
  let seen = null;
  for (;;) {
    try {
      seen = predicate();
    } catch (error) {
      seen = null;
    }
    if (seen) return seen;
    if (Date.now() >= deadline) throw new StepFailed(`timed out after ${timeoutMs}ms waiting for ${label}`);
    await new Promise((resolveFn) => setTimeout(resolveFn, intervalMs));
  }
}

/**
 * What a failing step leaves behind: the state of the host and the wire at the
 * moment it failed. Read from memory and the captured streams only — a failed
 * step must not start another process or CLI call.
 */
function hostDiagnostics(host) {
  return {
    commandLine: host.commandLine ?? null,
    exit: host.exit,
    mountLines: host.mountLines,
    pluginLogTail: host.stderrLines.filter((line) => line.startsWith("[pi-onlyne]")).slice(-20),
    stderrTail: host.stderrLines.slice(-20),
    pluginErrors: host.pluginErrors,
    frameTypes: host.frames.slice(-12).map((frame) => frame.type),
    lastPromptResults: host.frames.filter((frame) => frame.type === "prompt_result").slice(-3),
    lastAssistantText: host.frames
      .filter((frame) => frame.type === "message_end" && frame.message?.role === "assistant")
      .slice(-2)
      .map((frame) => stringsIn(frame.message?.content).join(" ").slice(0, 500)),
  };
}

async function step(name, body, { keepGoing, scenario = false }) {
  // `--smoke` keeps the wiring (spawn, mount, teardown) and drops every step
  // that would spend a model turn. Marked skipped, never counted as passed.
  if (scenario && report.smoke) {
    report.steps.push({ name, ok: true, skipped: true, at: new Date().toISOString(), evidence: { reason: "--smoke" } });
    process.stdout.write(`skip ${name} (--smoke)\n`);
    return null;
  }
  try {
    const evidence = await body();
    record(name, evidence, true);
    return evidence;
  } catch (error) {
    const failure = error instanceof StepFailed ? error.message : String(error?.stack ?? error);
    record(name, error.evidence ?? report.stepDiagnostics?.() ?? null, false, failure);
    if (keepGoing) return null;
    throw error;
  }
}

async function main() {
  const options = resolveOptions(process.argv.slice(2));
  if (options.evidenceDir) mkdirSync(options.evidenceDir, { recursive: true });
  report.options = sanitizeOptions(options);

  const extensionDir = existsSync(join(options.pluginPath, "src", "index.ts"))
    ? join(options.pluginPath, "src")
    : options.pluginPath;

  // ---- preflight ---------------------------------------------------------
  await step("preflight", async () => {
    const problems = [];
    // The body root is this driver's own directory — it holds `.pi/onlyne.json`
    // and the host's session files — so a missing one is created rather than
    // refused. Recorded either way, because a typo'd --cwd shows up here.
    let createdCwd = false;
    if (!existsSync(options.cwd)) {
      mkdirSync(options.cwd, { recursive: true });
      createdCwd = true;
    }
    if (!existsSync(join(options.pluginPath, "package.json"))) problems.push(`plugin missing: ${options.pluginPath}`);
    if (!existsSync(join(extensionDir, "index.ts"))) problems.push(`plugin entry missing: ${extensionDir}/index.ts`);
    if (!existsSync(options.onlyneBin) && !options.onlyneBin.includes("/")) problems.push(`onlyne CLI missing: ${options.onlyneBin}`);
    if (options.workspace && !existsSync(options.workspace)) problems.push(`workspace missing: ${options.workspace}`);

    let wroteConfig = null;
    if (!options.configPresent) {
      require(options.writeConfig, `.pi/onlyne.json is absent and --no-write-config was given`);
      if (!options.workspace) problems.push("cannot write .pi/onlyne.json without --workspace");
      if (problems.length === 0) {
        mkdirSync(dirname(options.configPath), { recursive: true });
        wroteConfig = residentConfigLine({ role: options.role, sessionId: options.sessionId, workspace: options.workspace });
        writeFileSync(options.configPath, wroteConfig);
      }
    }
    require(problems.length === 0, problems.join("; "));

    const workspaceConfig = options.workspace ? join(options.workspace, ".onlyne", "config.toml") : null;
    const evidence = {
      createdCwd,
      wroteConfig,
      config: readFileSync(options.configPath, "utf8"),
      workspaceConfigPresent: workspaceConfig ? existsSync(workspaceConfig) : false,
      workspaceConfig: workspaceConfig && existsSync(workspaceConfig) ? readFileSync(workspaceConfig, "utf8").slice(0, 2000) : null,
      extensionDir,
      runtimeDirExists: options.runtimeDir ? existsSync(options.runtimeDir) : null,
      ompVersion: spawnSync(options.ompBin, ["--version"], { encoding: "utf8" }).stdout?.trim() ?? null,
    };
    require(evidence.ompVersion, "omp did not answer --version");
    return evidence;
  }, { keepGoing: false });

  // ---- host + mount ------------------------------------------------------
  const host = new OmpHost({
    ...options,
    extensionDir,
    pluginRoot: options.pluginPath,
  });
  report.smoke = options.smoke;
  report.stepDiagnostics = () => hostDiagnostics(host);

  let sessionsRow = null;
  /** Whether this host's tool list shows extension tools at all (see parent-mounted). */
  let toolListCarriesOnlyne = false;
  const querySessions = () => {
    if (!options.serverRoot) return null;
    const args = ["--server-root", options.serverRoot, "--timeout", "20000", "sessions", "--role", options.role, "--json"];
    const result = runCli(options, args);
    if (!result.body) return { ...result, rows: [] };
    return { ...result, rows: sessionRows(result.body) };
  };

  const teardown = async () => {
    if (!host.child) return;
    host.kill("SIGTERM");
    await host.waitFor("process exit", (h) => h.exit !== null, options.timeouts.exit).catch(() => host.kill("SIGKILL"));
    writeFileSync(join(options.evidenceDir, "host.stdout.ndjson"), host.frames.map((f) => JSON.stringify(f)).join("\n"));
    writeFileSync(join(options.evidenceDir, "host.stderr.log"), `${host.stderrLines.join("\n")}\n`);
  };

  try {
    host.start();
    await step("host-ready", async () => {
      await host.waitFor("the ready frame", (h) => h.frames.some((f) => f.type === "ready"), options.timeouts.ready);
      const ready = host.frames.find((f) => f.type === "ready");
      const negotiated = await host.request("negotiate_protocol", { protocolVersion: 2 }, 10_000);
      require(negotiated.success === true, `negotiate_protocol failed: ${negotiated.error ?? "unknown"}`);
      return { commandLine: host.commandLine, ready, negotiated: negotiated.data };
    }, { keepGoing: options.keepGoing });

    await step("parent-mounted", async () => {
      await host.waitFor("the plugin's mount line", (h) => h.mountLines.length > 0, options.timeouts.mount);
      const [, lineSession, lineRole, lineSocket] = MOUNT_LINE.exec(host.mountLines[0]);
      require(lineRole === options.role, `mount line names role ${lineRole}, expected ${options.role}`);
      require(host.mountLines.length === 1, `expected one mount line, saw ${host.mountLines.length}`);
      require(host.pluginErrors.length === 0, `plugin raised ${host.pluginErrors.length} extension_error frames`);

      const state = await host.request("get_state", {}, 20_000);
      require(state.success === true, `get_state failed: ${state.error}`);
      sessionsRow = querySessions();
      const row = sessionsRow?.rows?.find((candidate) => candidate.role === options.role) ?? sessionsRow?.rows?.[0] ?? null;
      require(row !== null, `no session row for role ${options.role}: ${sessionsRow?.stdout ?? "no --server-root given"}`);
      require(row.lifecycle !== "exited", `session row for ${options.role} already reads exited`);
      // The mounted parent is the baseline for reading a child's tool list: a
      // host that hides extension tools from `dumpTools` makes the child's list
      // unable to answer "did the child register onlyne tools", and the report
      // must say so instead of presenting that list as proof.
      const hostTools = Array.isArray(state.data?.dumpTools) ? state.data.dumpTools.map((tool) => tool.name) : [];
      toolListCarriesOnlyne = hostTools.some((name) => String(name).startsWith("onlyne_"));
      return {
        mountLine: host.mountLines[0],
        wireSessionId: lineSession,
        socket: lineSocket,
        extensionErrors: host.pluginErrors.length,
        hostTools,
        toolListCarriesOnlyne,
        hostState: { sessionId: state.data?.sessionId, model: state.data?.model?.id, provider: state.data?.model?.provider, isSettled: state.data?.isSettled },
        sessionRow: row,
      };
    }, { keepGoing: options.keepGoing });

    // ---- a real task child, spawned and disposed --------------------------
    const childMarker = `${options.marker}-child`;
    let subagent = null;
    await step("task-child-disposed", async () => {
      const before = host.mountLines.length;
      const stateBefore = await host.request("get_state", {}, 20_000);
      await host.request("set_subagent_subscription", { level: "events" }, 10_000).catch(() => null);

      const roster = taskRoster(stateBefore.data);
      const agent = options.taskAgent ?? roster[0] ?? null;
      require(agent !== null, "the task tool description named no agent to spawn");
      const directive = taskDirective({ agent, childMarker, isolated: options.taskIsolated });

      const promptId = `prompt-${Date.now()}`;
      host.send({ id: promptId, type: "prompt", message: directive });
      await host.waitFor(
        "the task turn to settle",
        (h) => h.frames.some((f) => f.type === "prompt_result" && (f.id === promptId || f.agentInvoked !== undefined)),
        options.timeouts.turn,
      );
      const result = host.frames.filter((f) => f.type === "prompt_result").at(-1) ?? null;
      require(result && result.status === "completed", `prompt_result status ${result?.status ?? "missing"}: ${result?.error?.message ?? ""}`);

      // The child's identity comes from the lifecycle frames the host emitted
      // while it ran: `get_subagents` answers from a registry that drops a
      // finished child, so a post-hoc read there finds nothing to report.
      const lifecycle = new Map();
      for (const frame of host.frames.filter((candidate) => candidate.type === "subagent_lifecycle")) {
        const payload = frame.payload;
        if (!payload?.id) continue;
        lifecycle.set(payload.id, { ...(lifecycle.get(payload.id) ?? {}), ...payload });
      }
      const children = [...lifecycle.values()];
      require(children.length >= 1, "the host emitted no subagent_lifecycle frame for the child");
      subagent = children[0];
      require(
        typeof subagent.parentToolCallId === "string" && subagent.parentToolCallId.length > 0,
        "child lifecycle payload carries no parentToolCallId",
      );
      require(
        CHILD_TERMINAL_STATUSES.has(subagent.status),
        `child status is ${subagent.status} after its turn settled`,
      );

      const childEvidence = readChildSession(options, subagent, childMarker);
      require(childEvidence.init !== null, `no session_init found for the child (looked at ${childEvidence.searched.join(", ")})`);
      // The child's own `session_init.tools` is the registered set for that
      // session, and it carries extension-registered tools: a throwaway
      // extension's tool appeared there in a check on this machine (34 tools,
      // the extra one among them). So an absent `onlyne_*` entry is evidence
      // the child's factory never registered the Onlyne tools, not a list that
      // cannot show extension tools.
      require(childEvidence.onlyneTools.length === 0, `child session_init lists onlyne tools: ${childEvidence.onlyneTools.join(", ")}`);

      // The plugin's own line names the child's ancestry: it saw a subagent
      // session reach its factory and stayed out. The wording belongs to the
      // plugin, so the pattern is a flag; the behavioural assertions above and
      // below are what the fix is judged on.
      const childInertLogLines = host.stderrLines.filter(
        (line) => line.startsWith("[pi-onlyne]") && new RegExp(options.inertPattern).test(line),
      );
      if (options.requireInertLog) {
        require(
          childInertLogLines.length >= 1,
          `the plugin logged no line matching /${options.inertPattern}/ for the child (see host.stderr.log)`,
        );
      }

      await host.waitFor("frames and logs to settle", (h) => h.mountLines.length >= 1, 1_000).catch(() => null);
      const mountLinesAfter = host.mountLines.length;
      // The live registry prunes a finished child, so an empty list here is the
      // reclamation read from the host's own bookkeeping, not an assertion the
      // harness needs: the lifecycle frames above already named the child.
      const liveChildren = await host.request("get_subagents", {}, 20_000).catch(() => null);
      const liveChildIds = Array.isArray(liveChildren?.data?.subagents)
        ? liveChildren.data.subagents.map((child) => ({ id: child.id, status: child.status }))
        : null;
      const sessionsAfter = querySessions();
      const rowAfter = sessionsAfter?.rows?.find((candidate) => candidate.role === options.role) ?? null;
      if (rowAfter) require(rowAfter.lifecycle !== "exited", "the parent's session read exited while its task child ran");

      require(mountLinesAfter === before, `mount line count moved from ${before} to ${mountLinesAfter} across the child's life`);
      return {
        agent,
        directive,
        promptResult: { status: result.status, agentInvoked: result.agentInvoked },
        child: {
          id: subagent.id,
          agent: subagent.agent,
          status: subagent.status,
          detached: subagent.detached ?? null,
          sessionFile: subagent.sessionFile ?? null,
          parentToolCallId: subagent.parentToolCallId,
        },
        childLifecycleStatuses: children.map((child) => child.status),
        childToolCallIds: [...new Set(children.map((child) => child.parentToolCallId))],
        childSessionInit: childEvidence.init,
        childOnlyneTools: childEvidence.onlyneTools,
        childInertLogLines: childInertLogLines,
        inertLogRequired: options.requireInertLog,
        liveChildrenAfterDisposal: liveChildIds,
        mountLineCount: host.mountLines.length,
        toolListCarriesOnlyne,
        mountLines: host.mountLines,
        sessionRowAfter: rowAfter,
      };
    }, { keepGoing: options.keepGoing, scenario: true });

    // ---- a real note from the real CLI ------------------------------------
    let noteFrom = options.noteFrom ?? null;
    await step("note-received", async () => {
      noteFrom = noteFrom ?? pickNoteSender(options);
      require(noteFrom !== null && noteFrom !== options.role, `no sender role outside ${options.role} to send the note from`);
      const marker = `${options.marker}-note1`;
      const sent = sendNote(options, { from: noteFrom, to: options.role, text: `${marker}: acceptance note. Reply with exactly "ack ${marker}" and nothing else.` });
      // Recorded before the wait, so a note the CLI refused still shows what it
      // answered in the report of a failed step.
      report.noteSends = [...(report.noteSends ?? []), { phase: "before-replacement", from: noteFrom, marker, sent }];
      require(sent.code === 0, `onlyne send exited ${sent.code}: ${sent.stderr || sent.stdout}`);

      await host.waitFor("the note to land in the host", (h) => noteCarriers(h, marker).length > 0, options.timeouts.note);
      const carriers = noteCarriers(host, marker);
      const sessionsAfter = querySessions();
      const row = sessionsAfter?.rows?.find((candidate) => candidate.role === options.role) ?? null;
      if (row) require(row.lifecycle !== "exited", "the parent's session read exited after the note");
      return { from: noteFrom, send: sent, carriers, sessionRow: row };
    }, { keepGoing: options.keepGoing, scenario: true });

    // ---- top-level session replacement ------------------------------------
    let secondNoteFrom = null;
    await step("session-replaced", async () => {
      const before = await host.request("get_state", {}, 20_000);
      const mountsBefore = host.mountLines.length;
      const replaced = await host.request("new_session", {}, 30_000);
      require(replaced.success === true, `new_session failed: ${replaced.error}`);
      require(replaced.data?.cancelled === false, "new_session reported cancelled");

      const after = await host.request("get_state", {}, 20_000);
      require(after.success === true, `get_state after new_session failed: ${after.error}`);
      require(after.data.sessionId !== before.data.sessionId, `session id unchanged after new_session (${after.data.sessionId})`);
      require(host.mountLines.length === mountsBefore, `a mount line appeared across the replacement (${host.mountLines.length} vs ${mountsBefore})`);

      secondNoteFrom = noteFrom ?? pickNoteSender(options);
      const marker = `${options.marker}-note2`;
      const sent = sendNote(options, { from: secondNoteFrom, to: options.role, text: `${marker}: after replacement. Reply with exactly "ack ${marker}" and nothing else.` });
      report.noteSends = [...(report.noteSends ?? []), { phase: "after-replacement", from: secondNoteFrom, marker, sent }];
      require(sent.code === 0, `onlyne send after replacement exited ${sent.code}: ${sent.stderr || sent.stdout}`);
      await host.waitFor("the post-replacement note", (h) => noteCarriers(h, marker).length > 0, options.timeouts.note);

      const sessionsAfter = querySessions();
      const row = sessionsAfter?.rows?.find((candidate) => candidate.role === options.role) ?? null;
      if (row) require(row.lifecycle !== "exited", "the parent's session read exited across the replacement");
      return {
        sessionIdBefore: before.data.sessionId,
        sessionIdAfter: after.data.sessionId,
        sessionFileBefore: before.data.sessionFile ?? null,
        sessionFileAfter: after.data.sessionFile ?? null,
        mountLines: host.mountLines.length,
        noteSend: sent,
        carriers: noteCarriers(host, marker),
        sessionRow: row,
      };
    }, { keepGoing: options.keepGoing, scenario: true });

    // ---- real teardown ----------------------------------------------------
    await step("parent-teardown", async () => {
      const mountsBefore = host.mountLines.length;
      host.kill("SIGTERM");
      await host.waitFor("the host process to leave", (h) => h.exit !== null, options.timeouts.exit);
      require(host.mountLines.length === mountsBefore, `a mount line appeared during teardown`);
      await pollUntil(
        "the session row to read exited",
        () => {
          const rows = querySessions()?.rows ?? [];
          const row = rows.find((candidate) => candidate.role === options.role) ?? null;
          return row !== null && row.lifecycle === "exited";
        },
        options.timeouts.exit,
      );
      const finalSessions = querySessions();
      return {
        exit: host.exit,
        mountLines: host.mountLines,
        pluginLogTail: host.stderrLines.slice(-12),
        sessionRow: finalSessions?.rows?.find((candidate) => candidate.role === options.role) ?? null,
        sessionsAnswer: finalSessions,
      };
    }, { keepGoing: options.keepGoing });
  } finally {
    if (!host.exit) {
      await teardown();
      // One more session read after the real exit, so a failure still reports it.
      if (options.serverRoot) {
        report.postTeardownSessions = querySessions();
      }
    }
    report.endedAt = new Date().toISOString();
    report.framesByType = host.frames.reduce((acc, frame) => {
      acc[frame.type] = (acc[frame.type] ?? 0) + 1;
      return acc;
    }, {});
    report.exit = host.exit;
    report.ok = report.failures.length === 0;
    writeFileSync(join(options.evidenceDir, "host.stdout.ndjson"), host.frames.map((f) => JSON.stringify(f)).join("\n"));
    writeFileSync(join(options.evidenceDir, "host.stderr.log"), `${host.stderrLines.join("\n")}\n`);
    reportPath = join(options.evidenceDir, "report.json");
    writeFileSync(reportPath, `${JSON.stringify(report, null, 2)}\n`);
    process.stdout.write(`report: ${reportPath}\n`);
  }

  if (report.failures.length > 0) {
    process.stderr.write(`omp-live: ${report.failures.length} step(s) failed: ${report.failures.map((f) => f.name).join(", ")}\n`);
    process.exit(EXIT_FAILED);
  }
  process.stdout.write("omp-live: all steps passed\n");
}

// ---------------------------------------------------------------------------
// Scenario text and CLI calls
// ---------------------------------------------------------------------------

/** Agent names the task tool advertises, cheapest-looking first. */
function taskRoster(hostState) {
  const tools = Array.isArray(hostState?.dumpTools) ? hostState.dumpTools : [];
  const task = tools.find((tool) => tool.name === "task");
  const description = typeof task?.description === "string" ? task.description : "";
  const names = [];
  for (const match of description.matchAll(/`([a-z][a-z0-9_-]{1,30})`\s*:/g)) names.push(match[1]);
  const preferred = ["labor", "engineer", "theorist", "philosopher"];
  const ranked = preferred.filter((name) => names.includes(name));
  for (const name of names) if (!ranked.includes(name)) ranked.push(name);
  return ranked;
}

function taskDirective({ agent, childMarker, isolated }) {
  return [
    "Acceptance step for the onlyne lifecycle example. Do exactly this and nothing else:",
    "",
    "1. Call the `task` tool exactly once, with one item that spawns a real subagent:",
    `   - context: "Acceptance probe for the onlyne lifecycle example. Do not read, write or run anything; answer in one line."`,
    `   - task: "Reply with exactly ${childMarker} and nothing else."`,
    `   - solutionSpace: "trivial single-line reply; one expected answer"`,
    `   - agent: "${agent}"`,
    isolated ? "   - isolated: true" : "   - leave `isolated` unset.",
    "   Do not use `fork_task`, do not spawn more than one child.",
    "2. When the child returns, reply with exactly: TASK-OK",
    "",
    "Do not call any other tool. Do not use onlyne_send, onlyne_complete or onlyne_handoff.",
  ].join("\n");
}

/**
 * The role the note is sent from. The ACL decides it, not a guess: the parent's
 * own slice carries `allowed_senders`, and a sender outside that list is
 * refused with `acl_denied`. `_supervisor` reaches every role one-sided, so it
 * is the last resort rather than the first choice.
 */
function pickNoteSender(options) {
  const roles = runCli(options, ["--server-root", options.serverRoot, "--timeout", "20000", "roles", "--json"]);
  if (!roles.body) return null;
  const rows = new Map();
  const walk = (node) => {
    if (Array.isArray(node)) {
      for (const item of node) walk(item);
      return;
    }
    if (!node || typeof node !== "object") return;
    const role = typeof node.name === "string" ? node.name : node.role;
    const looksLikeRoleRow =
      typeof role === "string" &&
      (Array.isArray(node.allowed_senders) || Array.isArray(node.allowed_targets) || typeof node.admin === "boolean");
    if (looksLikeRoleRow) {
      if (!rows.has(role)) rows.set(role, node);
      return;
    }
    for (const key of Object.keys(node)) walk(node[key]);
  };
  walk(roles.body);
  const parent = rows.get(options.role) ?? null;
  const senders = Array.isArray(parent?.allowed_senders) ? parent.allowed_senders.map(String) : [];
  const others = [...rows.keys()].filter((role) => role !== options.role);
  const usable = (role) => role !== undefined && role !== null && role !== options.role && rows.get(role)?.admin !== true;
  const chosen =
    (senders.includes("*") ? others.find(usable) : undefined) ??
    senders.find(usable) ??
    others.find(usable) ??
    (senders.includes("_supervisor") || rows.has("_supervisor") ? "_supervisor" : undefined) ??
    null;
  report.noteSenderChoice = {
    answer: roles.stdout,
    parentAllowedSenders: senders,
    roleRows: [...rows.keys()],
    chosen,
  };
  return chosen;
}

function sendNote(options, { from, to, text }) {
  return runCli(options, [
    "--server-root", options.serverRoot,
    "--timeout", "30000",
    "send",
    "--from", from,
    "--to", to,
    "--note",
    "--text", text,
    "--force",
    "--yes-i-am-supervisor-not-other-role",
  ]);
}

/** The child's own transcript: its `session_init` contract and tool list. */
function readChildSession(options, subagent, childMarker) {
  const searched = [];
  const candidates = [];
  if (typeof subagent.sessionFile === "string" && subagent.sessionFile) candidates.push(subagent.sessionFile);
  for (const file of sessionFiles(options.sessionDir)) candidates.push(file.path);
  for (const path of candidates) {
    if (!existsSync(path) || searched.includes(path)) continue;
    searched.push(path);
    let entries;
    try {
      entries = readJsonl(path);
    } catch {
      continue;
    }
    const inits = entries.filter((entry) => entry.type === "session_init");
    for (const entry of inits) {
      const tools = Array.isArray(entry.tools) ? entry.tools.map(String) : [];
      const isChild = tools.length > 0 && (path === subagent.sessionFile || String(entry.task ?? "").includes(childMarker));
      if (!isChild) continue;
      return {
        searched,
        path,
        init: {
          agent: entry.agent ?? null,
          task: String(entry.task ?? "").slice(0, 400),
          toolCount: tools.length,
          tools,
          isolated: entry.isolated ?? null,
          resolvedModel: entry.resolvedModel ?? null,
        },
        onlyneTools: tools.filter((name) => name.startsWith("onlyne_")),
      };
    }
  }
  return { searched, path: null, init: null, onlyneTools: [] };
}

main().catch((error) => {
  if (report.failures.length === 0 && !(error instanceof StepFailed)) {
    report.crash = String(error?.stack ?? error);
    process.stderr.write(`omp-live: ${report.crash}\n`);
  } else if (report.failures.length > 0) {
    process.stderr.write(`omp-live: ${report.failures.length} step(s) failed: ${report.failures.map((f) => f.name).join(", ")}\n`);
  }
  if (reportPath) {
    report.ok = false;
    report.endedAt = new Date().toISOString();
    writeFileSync(reportPath, `${JSON.stringify(report, null, 2)}\n`);
  }
  process.exit(EXIT_FAILED);
});
