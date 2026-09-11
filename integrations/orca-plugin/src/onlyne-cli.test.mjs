// The session-row shape is taken from the repository's own wire vector
// (crates/onlyne-proto/tests/wire_vectors/res_session_row.json) so this suite
// fails if the backend contract drifts away from what the plugin parses.

import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { clientSocketPath, createOnlyneCli, normalizeSessionRow } from "./onlyne-cli.mjs";
import { createRunner } from "./runner.mjs";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const vectorPath = join(repoRoot, "crates/onlyne-proto/tests/wire_vectors/res_session_row.json");
const vector = existsSync(vectorPath) ? JSON.parse(readFileSync(vectorPath, "utf8")) : null;

function wireVectorRow() {
  const frame = JSON.parse(vector.frame);
  return frame.data;
}

function cliWith({ stdout, throws = null }) {
  const calls = [];
  const runner = createRunner({
    exec: async (binary, args) => {
      calls.push([binary, ...args].join(" "));
      if (throws) {
        const error = new Error("failed");
        error.code = throws.code ?? 1;
        error.stdout = throws.stdout ?? "";
        error.stderr = throws.stderr ?? "";
        throw error;
      }
      return { stdout, stderr: "" };
    },
  });
  return { cli: createOnlyneCli({ runner, binary: "onlyne" }), calls };
}

test("the client socket lives at <workspace>/.onlyne/run/s", () => {
  assert.equal(clientSocketPath("/tmp/ws"), join("/tmp/ws", ".onlyne/run/s"));
});

test("normalizes the authoritative session row from the repo wire vector", { skip: !vector }, () => {
  const session = normalizeSessionRow(wireVectorRow());
  assert.equal(session.taskId, "11111111-1111-4111-8111-111111111111");
  assert.equal(session.sessionId, "8b1c");
  assert.equal(session.role, "builder");
  assert.equal(session.lifecycle, "exited");
  assert.equal(session.agent, "gone");
  assert.equal(session.delivery, "accepted");
  assert.equal(session.resource, "closed");
  assert.equal(session.outcome, "done");
  assert.equal(session.updatedAt, "2026-09-10T12:00:00Z");
});

test("queries the workspace socket with the sessions verb and parses the res body", { skip: !vector }, async () => {
  const row = wireVectorRow();
  const stdout = JSON.stringify({ ok: true, data: { sessions: [row] } });
  const { cli, calls } = cliWith({ stdout });
  const result = await cli.querySessions("/tmp/ws/.onlyne/run/s");
  assert.equal(result.ok, true);
  assert.equal(result.sessions.length, 1);
  assert.equal(result.sessions[0].taskId, row.task_id);
  assert.deepEqual(calls, [
    "onlyne --socket /tmp/ws/.onlyne/run/s --as client sessions --json",
  ]);
});

test("a role filter is passed through", async () => {
  const { cli, calls } = cliWith({ stdout: JSON.stringify({ ok: true, data: { sessions: [] } }) });
  await cli.querySessions("/tmp/ws/.onlyne/run/s", { role: "planner" });
  assert.deepEqual(calls, [
    "onlyne --socket /tmp/ws/.onlyne/run/s --as client sessions --json --role planner",
  ]);
});

test("the --quiet payload shape is accepted too", async () => {
  const { cli } = cliWith({ stdout: JSON.stringify({ sessions: [] }) });
  const result = await cli.querySessions("/tmp/ws/.onlyne/run/s");
  assert.equal(result.ok, true);
  assert.deepEqual(result.sessions, []);
});

test("a CLI that does not know the sessions verb reports a surface mismatch", async () => {
  const { cli } = cliWith({
    stdout: "",
    throws: {
      code: 2,
      stdout: "",
      stderr: "error: unexpected argument '--socket' found\n\nUsage: onlyne [OPTIONS] <COMMAND>",
    },
  });
  const result = await cli.querySessions("/tmp/ws/.onlyne/run/s");
  assert.equal(result.ok, false);
  assert.equal(result.code, "cli_surface_mismatch");
});

test("a closed client link degrades as cli_error rather than throwing", async () => {
  const { cli } = cliWith({
    stdout: "",
    throws: { code: 1, stdout: JSON.stringify({ ok: false, error: { code: "internal", message: "not ready" } }), stderr: "" },
  });
  const result = await cli.querySessions("/tmp/ws/.onlyne/run/s");
  assert.equal(result.ok, false);
  assert.equal(result.code, "internal");
  assert.equal(result.message, "not ready");
});

test("a missing onlyne binary is reported, not thrown", async () => {
  const { cli } = cliWith({ stdout: "", throws: { code: "ENOENT" } });
  const result = await cli.querySessions("/tmp/ws/.onlyne/run/s");
  assert.equal(result.ok, false);
  assert.equal(result.code, "missing_binary");
});

test("an answer without session rows is reported as an unexpected shape", async () => {
  const { cli } = cliWith({ stdout: JSON.stringify({ ok: true, data: { roles: [] } }) });
  const result = await cli.querySessions("/tmp/ws/.onlyne/run/s");
  assert.equal(result.ok, false);
  assert.equal(result.code, "unexpected_shape");
});
