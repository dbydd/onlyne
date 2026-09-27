// Which socket path a pi session dials.
//
// The two answers have one order: the path the client injected, then the client
// the runtime directory's registration files name for this workspace. Nothing
// falls back to a path inside the tree — v2 binds nothing there, so a test that
// expected `<workspace>/.onlyne/run/s` would pin a socket no daemon holds.

import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, test } from "node:test";

import {
  SOCKET_ENV,
  SocketResolutionError,
  canonicalRoot,
  listClientRegistrations,
  registrationPath,
  resolveSocketPath,
  runtimeDir,
  socketPath,
  workspaceDigest,
} from "./socket.mjs";

const cleanups = [];
afterEach(() => {
  while (cleanups.length > 0) cleanups.pop()();
});

/** One temp directory, removed after the test. */
function scratch(prefix) {
  const dir = mkdtempSync(join(tmpdir(), prefix));
  cleanups.push(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

/** One temp runtime directory holding the registrations written into it. */
function runtime(registrations = []) {
  const dir = scratch("pi-onlyne-runtime-");
  for (const { name, body } of registrations) {
    writeFileSync(join(dir, name), typeof body === "string" ? body : JSON.stringify(body));
  }
  return dir;
}

/** A registration file's name for `root`, the leaf `onlyne-wire` derives. */
function registrationName(root) {
  return `${workspaceDigest(root)}.json`;
}

/** The registration of one client whose sessions run under `runtime`. */
function client(root, runtimeName = "pi") {
  // The writer's spelling is the canonical one (`RegistrationFile::new`).
  return { kind: "client", role: "planner", root: canonicalRoot(root), pid: 4242, version: "0.2.0", runtime: runtimeName };
}

/** The env a client-spawned session carries, with the runtime directory pinned. */
function envFor(dir, extra = {}) {
  return { ONLYNE_RUNTIME_DIR: dir, ...extra };
}

/** The refusal one resolve raises, which `assert.throws` does not hand back. */
function refusal(resolve) {
  try {
    resolve();
  } catch (error) {
    assert.ok(error instanceof SocketResolutionError, `expected a SocketResolutionError, got ${error}`);
    return error;
  }
  assert.fail("expected a SocketResolutionError");
}

test("the injected environment variable decides first", () => {
  const workspace = scratch("pi-onlyne-ws-");
  const dir = runtime([{ name: registrationName(workspace), body: client(workspace) }]);
  const injected = join(dir, "some-other.sock");

  assert.equal(resolveSocketPath(envFor(dir, { ONLYNE_SOCKET: injected }), workspace), injected);
  // A value wrapped in space names the same socket.
  assert.equal(resolveSocketPath(envFor(dir, { ONLYNE_SOCKET: `  ${injected}  ` }), workspace), injected);
  // It outranks a registration that answers for the same workspace.
  assert.notEqual(injected, socketPath(workspace, envFor(dir)));

  // A variable holding nothing answers nothing, so the registrations decide.
  assert.equal(resolveSocketPath(envFor(dir, { ONLYNE_SOCKET: "   " }), workspace), socketPath(workspace, envFor(dir)));
});

test("the digest is the reference's, sixteen hex characters of sha256 over the canonical root", () => {
  // Vectors from `crates/onlyne-wire/src/socket.rs::workspace_digest` and
  // `crates/onlyne-testkit/e2e/lib.sh::workspace_digest`: python3 -c
  // 'import hashlib;print(hashlib.sha256(PATH.encode()).hexdigest()[:16])'.
  assert.equal(workspaceDigest("/srv/onlyne"), "8bc9cceef0a8586d");
  assert.equal(workspaceDigest("/srv/onlyne/workspace"), "add3dd4d5bd2c91a");
  // Separators become `/` and the spelling is lowercased before hashing.
  assert.equal(workspaceDigest("/SRV/Onlyne"), "8bc9cceef0a8586d");

  // A root that exists is canonicalized, so one tree reached through two
  // spellings digests once.
  const real = scratch("pi-onlyne-ws-");
  const link = join(scratch("pi-onlyne-link-"), "ws");
  symlinkSync(real, link);
  assert.equal(workspaceDigest(link), workspaceDigest(real));
});

test("the runtime directory is the override, or /tmp/onlyne-<uid>", () => {
  assert.equal(runtimeDir({ ONLYNE_RUNTIME_DIR: "/tmp/pinned" }, { uid: 501 }), "/tmp/pinned");
  // An empty override is no override; a whitespace-only one is the reference's
  // business, and it is a directory name there rather than an empty value.
  assert.equal(runtimeDir({ ONLYNE_RUNTIME_DIR: "" }, { uid: 501 }), "/tmp/onlyne-501");
  assert.equal(runtimeDir({}, { uid: 501 }), "/tmp/onlyne-501");
  assert.equal(runtimeDir({}, { uid: 0 }), "/tmp/onlyne-0");
});

test("the socket is derived from the workspace root in the runtime directory", () => {
  const workspace = scratch("pi-onlyne-ws-");
  const dir = runtime([{ name: registrationName(workspace), body: client(workspace) }]);
  const env = envFor(dir);

  const resolved = resolveSocketPath(env, workspace);
  assert.equal(resolved, join(dir, `${workspaceDigest(workspace)}.sock`));
  assert.equal(resolved, socketPath(workspace, env));
  assert.equal(registrationPath(workspace, env), join(dir, registrationName(workspace)));
  // Nothing inside the workspace serves anything in v2.
  assert.ok(!resolved.startsWith(workspace));

  // The override moves the whole derivation, socket and registration alike.
  const pinned = runtime([{ name: registrationName(workspace), body: client(workspace) }]);
  assert.equal(resolveSocketPath(envFor(pinned), workspace), join(pinned, `${workspaceDigest(workspace)}.sock`));
  assert.equal(socketPath(workspace, envFor(pinned)), join(pinned, `${workspaceDigest(workspace)}.sock`));
});

test("a pi started below the workspace root resolves the workspace's client", () => {
  const workspace = scratch("pi-onlyne-ws-");
  const nested = join(workspace, ".onlyne", "workspaces", "topology", "planner");
  mkdirSync(nested, { recursive: true });
  const dir = runtime([{ name: registrationName(workspace), body: client(workspace) }]);

  assert.equal(resolveSocketPath(envFor(dir), nested), socketPath(workspace, envFor(dir)));
  // The nearest registered root wins over one that only contains it, so a
  // client of the outer tree never answers for a session inside the inner one.
  const outer = scratch("pi-onlyne-ws-");
  const inner = join(outer, "inner");
  mkdirSync(inner);
  const both = runtime([
    { name: registrationName(outer), body: client(outer) },
    { name: registrationName(inner), body: client(inner) },
  ]);
  assert.equal(resolveSocketPath({ ONLYNE_RUNTIME_DIR: both }, inner), socketPath(inner, { ONLYNE_RUNTIME_DIR: both }));
});

test("a registration that is not this plugin's surface is not an answer", () => {
  const workspace = scratch("pi-onlyne-ws-");
  // A server root's admin socket speaks the admin vocabulary, not the adapter
  // protocol, and `list_registrations` would hand it over just the same.
  const dir = runtime([
    { name: registrationName(workspace), body: { kind: "server", role: null, root: workspace, pid: 1, version: "0.2.0", runtime: null } },
    { name: "broken.json", body: "{ not a registration" },
    { name: "notes.txt", body: "not a registration at all" },
    { name: "rootless.json", body: { kind: "client", pid: 2 } },
  ]);

  assert.deepEqual(listClientRegistrations(envFor(dir)), []);
  const error = refusal(() => resolveSocketPath(envFor(dir), workspace));
  assert.match(error.message, /no client is registered for/);
  assert.match(error.message, new RegExp(registrationName(workspace).replace(/\./g, "\\.")));
  assert.match(error.message, /holds no client registration/);
});

test("a refused resolution names what was looked for", () => {
  const workspace = scratch("pi-onlyne-ws-");
  const empty = scratch("pi-onlyne-runtime-");
  const nowhere = refusal(() => resolveSocketPath(envFor(empty), workspace));
  assert.match(nowhere.message, new RegExp(workspaceDigest(workspace)));
  assert.match(nowhere.message, new RegExp(empty));
  assert.deepEqual(nowhere.candidates, []);

  // Another tree's client is reported by name rather than dialled.
  // An ACP client is another runtime's, so it is not this plugin's to dial and
  // the refusal names it instead.
  const other = scratch("pi-onlyne-ws-");
  const dir = runtime([{ name: registrationName(other), body: client(other, "acp") }]);
  const elsewhere = refusal(() => resolveSocketPath(envFor(dir), workspace));
  assert.match(elsewhere.message, new RegExp(canonicalRoot(other).replace(/\./g, "\\.")));
  assert.match(elsewhere.message, /belong to other roots/);
  assert.match(elsewhere.message, /runtime acp/);
});

test("several matching clients are refused, not picked from", () => {
  const workspace = scratch("pi-onlyne-ws-");
  const first = scratch("pi-onlyne-ws-");
  const second = scratch("pi-onlyne-ws-");

  // Two clients run pi sessions and neither root contains this workspace: the
  // external placement's discovery key has no single answer.
  const dir = runtime([
    { name: registrationName(first), body: client(first) },
    { name: registrationName(second), body: client(second) },
  ]);
  const error = refusal(() => resolveSocketPath(envFor(dir), workspace));
  assert.match(error.message, /name runtime pi/);
  assert.match(error.message, /ambiguous/);
  assert.equal(error.candidates.length, 2);

  // Two registrations naming one tree are not ambiguous: the socket is the
  // tree's digest, so both name one path and deduping is not a pick.
  const duplicated = runtime([
    { name: registrationName(workspace), body: client(workspace) },
    { name: "hand-written.json", body: client(workspace) },
  ]);
  assert.equal(resolveSocketPath(envFor(duplicated), workspace), socketPath(workspace, envFor(duplicated)));

  // One runtime match with no root of its own is the plan's external case, and
  // one answer is an answer.
  const single = runtime([{ name: registrationName(first), body: client(first) }]);
  const sole = resolveSocketPath(envFor(single), workspace);
  assert.equal(sole, socketPath(first, envFor(single)));
  assert.equal(sole, join(single, `${workspaceDigest(first)}.sock`));
});

test("a registration read through the injected filesystem names the same socket", () => {
  const workspace = "/srv/onlyne/workspace";
  const dir = "/tmp/onlyne-501";
  const options = {
    uid: 501,
    realpath: (path) => path,
    readDir: () => [registrationName(workspace), "unrelated.json"],
    readFile: (path) => (path.endsWith(registrationName(workspace)) ? JSON.stringify(client(workspace)) : "{}"),
  };

  assert.equal(
    resolveSocketPath({ ONLYNE_RUNTIME_DIR: dir }, workspace, options),
    `${dir}/add3dd4d5bd2c91a.sock`,
  );
  // The env is what names the socket a client injected, and it is still first.
  assert.equal(resolveSocketPath({ ONLYNE_RUNTIME_DIR: dir, [SOCKET_ENV]: "/tmp/served" }, workspace, options), "/tmp/served");
});
