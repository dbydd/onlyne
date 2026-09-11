import assert from "node:assert/strict";
import { test } from "node:test";
import {
  createRunner,
  normalizePiWorkspaces,
  normalizeServerRoots,
  parseCliJson,
  readPluginConfig,
  resolveBinary,
  resolveBinaries,
} from "./runner.mjs";

test("parses the Orca CLI failure body that arrives on stdout with exit 1", async () => {
  const stale = JSON.stringify({ ok: false, error: { code: "terminal_handle_stale", message: "no such terminal" } });
  const runner = createRunner({
    exec: async () => {
      const error = new Error("Command failed");
      error.code = 1;
      error.stdout = stale;
      error.stderr = "";
      throw error;
    },
  });
  const result = await runner.runJson("orca", ["terminal", "show", "--json"]);
  assert.equal(result.ok, false);
  assert.equal(result.code, "terminal_handle_stale");
  assert.equal(result.message, "no such terminal");
});

test("a missing binary is reported as missing_binary, not as a JSON error", async () => {
  const runner = createRunner({
    exec: async () => {
      const error = new Error("spawn onlyne ENOENT");
      error.code = "ENOENT";
      error.stdout = "";
      error.stderr = "";
      throw error;
    },
  });
  const result = await runner.runJson("onlyne", ["sessions"]);
  assert.equal(result.ok, false);
  assert.equal(result.code, "missing_binary");
});

test("unwraps the {id, ok, result} envelope the orca CLI prints", async () => {
  const runner = createRunner({
    exec: async () => ({
      stdout: JSON.stringify({ id: "x", ok: true, result: { worktrees: [{ path: "/tmp/a" }] } }),
      stderr: "",
    }),
  });
  const result = await runner.runJson("orca", ["worktree", "list", "--json"]);
  assert.equal(result.ok, true);
  assert.deepEqual(result.value, { worktrees: [{ path: "/tmp/a" }] });
});

test("non-JSON output fails loudly instead of being treated as data", () => {
  const parsed = parseCliJson("usage: onlyne <COMMAND>");
  assert.equal(parsed.ok, false);
  assert.equal(parsed.code, "bad_json");
  assert.equal(parseCliJson("").code, "empty_output");
});

test("resolveBinary prefers an existing candidate over the bare name", () => {
  const resolved = resolveBinary("orca", {
    home: "/home/tester",
    exists: (path) => path === "/opt/homebrew/bin/orca",
    candidates: ["/opt/homebrew/bin/orca"],
  });
  assert.equal(resolved, "/opt/homebrew/bin/orca");
  assert.equal(resolveBinary("orca", { home: "/home/tester", exists: () => false }), "orca");
});

test("a malformed operator config degrades to zero-config", () => {
  const result = readPluginConfig({
    home: "/home/tester",
    exists: () => true,
    readFile: () => "{ not json",
  });
  assert.equal(result.loaded, false);
  assert.ok(result.error);
});

test("BIN_DIR pins the freshly built binaries, and only when they exist", () => {
  const present = new Set(["/repo/target/debug/onlyne", "/opt/homebrew/bin/orca"]);
  const resolved = resolveBinaries({
    home: "/home/tester",
    readFile: () => "{}",
    env: { BIN_DIR: "/repo/target/debug" },
    exists: (path) => present.has(path),
  });
  assert.equal(resolved.onlyneBin, "/repo/target/debug/onlyne");
  // `orca` is not in the build directory, so discovery still finds the app CLI.
  assert.equal(resolved.orcaBin, "/opt/homebrew/bin/orca");
  assert.equal(resolved.binDir, "/repo/target/debug");

  const absent = resolveBinaries({
    home: "/home/tester",
    readFile: () => "{}",
    env: { BIN_DIR: "/repo/target/debug" },
    exists: (path) => path === "/home/tester/.local/bin/onlyne",
  });
  assert.equal(absent.onlyneBin, "/home/tester/.local/bin/onlyne");
});

test("an operator config outranks BIN_DIR", () => {
  const resolved = resolveBinaries({
    home: "/home/tester",
    readFile: () => JSON.stringify({ onlyneBin: "/custom/onlyne", orcaBin: "/custom/orca" }),
    env: { BIN_DIR: "/repo/target/debug" },
    exists: () => true,
  });
  assert.equal(resolved.onlyneBin, "/custom/onlyne");
  assert.equal(resolved.orcaBin, "/custom/orca");
});

test("configured path lists keep entries trimmed and deduped", () => {
  assert.deepEqual(normalizeServerRoots(["/srv/a", "  /srv/b  ", "/srv/a", 42, "", "   "]), [
    "/srv/a",
    "/srv/b",
  ]);
  assert.deepEqual(normalizeServerRoots(undefined), []);
  assert.deepEqual(normalizeServerRoots(" /srv/a "), []);
  assert.deepEqual(normalizePiWorkspaces(["/ws/a", "/ws/a", null]), ["/ws/a"]);
});

test("the operator config supplies serverRoots and piWorkspaces beside the binaries", () => {
  const resolved = resolveBinaries({
    home: "/home/tester",
    readFile: () =>
      JSON.stringify({
        serverRoots: ["/srv/a"],
        piWorkspaces: ["/ws/planner"],
        orcaBin: "/custom/orca",
      }),
    env: {},
    exists: () => true,
  });
  assert.deepEqual(resolved.serverRoots, ["/srv/a"]);
  // The two lists are independent: a workspace is not a server root.
  assert.deepEqual(resolved.piWorkspaces, ["/ws/planner"]);
  assert.equal(resolved.orcaBin, "/custom/orca");

  const bare = resolveBinaries({ home: "/home/tester", readFile: () => "{}", env: {}, exists: () => true });
  assert.deepEqual(bare.serverRoots, []);
  assert.deepEqual(bare.piWorkspaces, []);
});
