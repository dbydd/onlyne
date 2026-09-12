// The session/role row shapes are taken from the repository's own wire vectors
// (crates/onlyne-proto/tests/wire_vectors/) so this suite fails if the backend
// contract drifts away from what the plugin parses.

import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { createOnlyneCli, normalizeRoleRow, normalizeSessionRow } from "./onlyne-cli.mjs";
import { createRunner } from "./runner.mjs";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const vectorsRoot = join(repoRoot, "crates/onlyne-proto/tests/wire_vectors");

function loadVector(name) {
  const path = join(vectorsRoot, name);
  return existsSync(path) ? JSON.parse(readFileSync(path, "utf8")) : null;
}

const sessionVector = loadVector("res_session_row.json");
const roleVector = loadVector("res_role_info.json");

function vectorRow(vector) {
  return JSON.parse(vector.frame).data;
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

test("normalizes the authoritative session row from the repo wire vector", { skip: !sessionVector }, () => {
  const session = normalizeSessionRow(vectorRow(sessionVector));
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

test("normalizes the roles row from the repo wire vector", { skip: !roleVector }, () => {
  const role = normalizeRoleRow(vectorRow(roleVector));
  assert.equal(role.role, "planner");
  assert.equal(role.presence, "draining");
  assert.equal(role.sessions, 2);
  assert.equal(role.admin, false);
  assert.equal(role.maxSessions, 3);
});

test("queries a server root's admin socket with the sessions verb", { skip: !sessionVector }, async () => {
  const row = vectorRow(sessionVector);
  const stdout = JSON.stringify({ ok: true, data: { sessions: [row] } });
  const { cli, calls } = cliWith({ stdout });
  const result = await cli.querySessions("/srv/onlyne-a");
  assert.equal(result.ok, true);
  assert.equal(result.sessions.length, 1);
  assert.equal(result.sessions[0].taskId, row.task_id);
  assert.deepEqual(calls, ["onlyne --server-root /srv/onlyne-a sessions --json"]);
});

test("queries the roles verb of the same root", { skip: !roleVector }, async () => {
  const stdout = JSON.stringify({ ok: true, data: { roles: [vectorRow(roleVector)] } });
  const { cli, calls } = cliWith({ stdout });
  const result = await cli.queryRoles("/srv/onlyne-a");
  assert.equal(result.ok, true);
  assert.deepEqual(
    result.roles.map((role) => `${role.role}/${role.presence}`),
    ["planner/draining"]
  );
  assert.deepEqual(calls, ["onlyne --server-root /srv/onlyne-a roles --json"]);
});

test("the --quiet payload shape is accepted too", async () => {
  const { cli } = cliWith({ stdout: JSON.stringify({ sessions: [] }) });
  const result = await cli.querySessions("/srv/onlyne-a");
  assert.equal(result.ok, true);
  assert.deepEqual(result.sessions, []);
});

test("a CLI that does not know the verb reports a surface mismatch", async () => {
  const { cli } = cliWith({
    stdout: "",
    throws: {
      code: 2,
      stdout: "",
      stderr: "error: unexpected argument '--server-root' found\n\nUsage: onlyne [OPTIONS] <COMMAND>",
    },
  });
  const result = await cli.querySessions("/srv/onlyne-a");
  assert.equal(result.ok, false);
  assert.equal(result.code, "cli_surface_mismatch");
});

test("an absent server socket degrades as cli_error with the canonical hint", async () => {
  const { cli } = cliWith({
    stdout: "",
    throws: {
      code: 3,
      stdout: "",
      stderr: "onlyne: no onlyne socket found; pass --socket, --server-root, or --workspace",
    },
  });
  const result = await cli.querySessions("/srv/onlyne-a");
  assert.equal(result.ok, false);
  assert.equal(result.code, "cli_error");
  assert.match(result.message, /no onlyne socket found/);
});

test("a server-side failure body degrades with its own code", async () => {
  const { cli } = cliWith({
    stdout: "",
    throws: { code: 1, stdout: JSON.stringify({ ok: false, error: { code: "internal", message: "not ready" } }), stderr: "" },
  });
  const result = await cli.queryRoles("/srv/onlyne-a");
  assert.equal(result.ok, false);
  assert.equal(result.code, "internal");
  assert.equal(result.message, "not ready");
});

test("a missing onlyne binary is reported, not thrown", async () => {
  const { cli } = cliWith({ stdout: "", throws: { code: "ENOENT" } });
  const result = await cli.querySessions("/srv/onlyne-a");
  assert.equal(result.ok, false);
  assert.equal(result.code, "missing_binary");
});

test("an answer without the verb's rows is reported as an unexpected shape", async () => {
  const { cli } = cliWith({ stdout: JSON.stringify({ ok: true, data: { roles: [] } }) });
  const result = await cli.querySessions("/srv/onlyne-a");
  assert.equal(result.ok, false);
  assert.equal(result.code, "unexpected_shape");
});
