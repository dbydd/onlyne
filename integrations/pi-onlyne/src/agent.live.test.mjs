// Integration: the plugin against a really-running `onlyne-client`.
//
// Everything else in this suite talks to a fake host built from the same
// framing code, which cannot catch a disagreement with the shipped binary. This
// case starts the real client's adapter socket (a workspace from
// `onlyne-client init`), opens the handshake, and reads the welcome the Rust
// side actually writes.
//
// The client's adapter socket binds before its server link, so no server and no
// task are needed to prove the live handshake. When the binaries are not built
// the case skips rather than failing: it verifies a build artifact, not source.

import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

import { OnlyneAgent } from "./agent.mjs";

const REPO_ROOT = fileURLToPath(new URL("../../..", import.meta.url));
const BIN_DIR = join(REPO_ROOT, "target", "debug");
const CLIENT = join(BIN_DIR, "onlyne-client");
const SERVER = join(BIN_DIR, "onlyne-server");
const hasBinaries = existsSync(CLIENT) && existsSync(SERVER);
const TASK_ID = "11111111-1111-4111-8111-111111111111";
const SESSION_ID = "8b1c0d5e-2222-4222-8222-222222222222";

/** Wait until `predicate` holds; throws when the window closes. */
async function waitFor(predicate, { timeoutMs = 15_000, stepMs = 20 } = {}) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = await predicate();
    if (value) return value;
    if (Date.now() > deadline) throw new Error("timed out waiting for the real client");
    await new Promise((resolve) => setTimeout(resolve, stepMs));
  }
}

test("a real onlyne-client answers hello with a welcome", { skip: !hasBinaries }, async () => {
  const tmp = mkdtempSync(join(tmpdir(), "pi-onlyne-live-"));
  const serverRoot = join(tmp, "server");
  const workspace = join(tmp, "planner");
  // `init` reads the server's spec.toml for the listen address and cert pin, so
  // the server root is bootstrapped first; the server itself never runs, which
  // is the point: the adapter socket is served independently of the link.
  execFileSync(SERVER, ["init", "--root", serverRoot, "--listen", "127.0.0.1:7899"], { stdio: "pipe" });
  execFileSync(CLIENT, ["init", "--workspace", workspace, "--role", "planner", "--server-root", serverRoot], {
    stdio: "pipe",
  });
  const child = spawn(CLIENT, ["run", "--workspace", workspace], { stdio: ["ignore", "pipe", "pipe"] });
  let clientLog = "";
  child.stdout.on("data", (chunk) => { clientLog += chunk; });
  child.stderr.on("data", (chunk) => { clientLog += chunk; });

  const socketPath = join(workspace, ".onlyne", "run", "s");
  const surface = {
    available: { wakeUser: true },
    calls: [],
    wakeUser(text) { this.calls.push(text); return true; },
    proseContext: () => true,
    customEntry: () => true,
    status: () => {},
    welcome: () => {},
    isIdle: () => true,
    exit: () => {},
  };
  const logs = [];
  const agent = new OnlyneAgent({
    socketPath,
    cwd: workspace,
    role: "planner",
    sessionId: SESSION_ID,
    taskId: TASK_ID,
    surface,
    log: (line) => logs.push(line),
    heartbeatMs: 60_000,
  });
  try {
    await waitFor(() => existsSync(socketPath));
    agent.start();
    await waitFor(() => agent.status().connected, { timeoutMs: 15_000 });

    const welcome = agent.welcome;
    assert.equal(welcome.role, "planner", `client log: ${clientLog}`);
    assert.equal(welcome.protocol, 1);
    assert.equal(welcome.generation, 1);
    assert.equal(welcome.sessionId, SESSION_ID, "the client echoes the mounted session");
    assert.deepEqual(welcome.hostCapabilities, ["probe", "recycle"]);
    assert.equal(typeof welcome.server.connected, "boolean");

    // The client refuses a `ready` for a task it never staged, and that refusal
    // is the proof the frame loop round-trips: it is a reply to a request this
    // plugin sent after welcome, carrying a real error payload.
    await waitFor(() => logs.some((line) => line.includes("ready refused: internal: unknown session for")), {
      timeoutMs: 10_000,
    });
    assert.equal(agent.status().lastError, null);

  } finally {
    agent.stop("test");
    child.kill("SIGTERM");
    await Promise.race([
      new Promise((resolve) => child.once("exit", resolve)),
      new Promise((resolve) => setTimeout(resolve, 3_000)),
    ]);
    if (child.exitCode === null) child.kill("SIGKILL");
    rmSync(tmp, { recursive: true, force: true });
  }
});
