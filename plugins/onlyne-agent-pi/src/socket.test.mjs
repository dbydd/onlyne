// Which socket path a pi session dials.
//
// The three answers have to stay in one order: what the client injected, what
// the daemon published in `<run>/socket`, and the canonical `<run>/s`. A
// workspace deep enough that the canonical spelling passes macOS' 103-byte
// `sun_path` bound is served from a short path under the temporary directory,
// and the marker is the only place inside the tree that names it.

import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, test } from "node:test";

import { SOCKET_MARKER_RELATIVE_PATH, SOCKET_RELATIVE_PATH, resolveSocketPath } from "./socket.mjs";

const cleanups = [];
afterEach(() => {
  while (cleanups.length > 0) cleanups.pop()();
});

/** One temp workspace whose `run/` directory exists, optionally with a marker body. */
function workspace(markerBody) {
  const dir = mkdtempSync(join(tmpdir(), "pi-onlyne-socket-"));
  cleanups.push(() => rmSync(dir, { recursive: true, force: true }));
  mkdirSync(join(dir, ".onlyne", "run"), { recursive: true });
  if (markerBody !== undefined) writeFileSync(join(dir, SOCKET_MARKER_RELATIVE_PATH), markerBody);
  return dir;
}

/** The short path a daemon serves an over-long workspace from. */
const SERVED = join(tmpdir(), "onlyne-0123456789abcdef", "s");

test("the injected environment variable decides first", () => {
  const dir = workspace(`${SERVED}\n`);
  assert.equal(resolveSocketPath({ ONLYNE_SOCKET: SERVED }, dir), SERVED);
  // Any other published answer loses to the environment.
  assert.equal(resolveSocketPath({ ONLYNE_SOCKET: join(dir, SOCKET_RELATIVE_PATH) }, dir), join(dir, SOCKET_RELATIVE_PATH));
  // A value wrapped in space names the same socket.
  assert.equal(resolveSocketPath({ ONLYNE_SOCKET: `  ${SERVED}  ` }, dir), SERVED);

  // A variable holding nothing answers nothing, so the tree decides.
  const bare = workspace();
  assert.equal(resolveSocketPath({ ONLYNE_SOCKET: "   " }, bare), join(bare, SOCKET_RELATIVE_PATH));
  assert.equal(resolveSocketPath({}, bare), join(bare, SOCKET_RELATIVE_PATH));
});

test("a published marker moves the session onto the served path", () => {
  const dir = workspace(`${SERVED}\n`);
  const resolved = resolveSocketPath({}, dir);
  assert.equal(resolved, SERVED);
  // The moved path lives outside the workspace, which is the whole point of the
  // marker: the tree's own `run/s` would be refused by the kernel here.
  assert.ok(!resolved.startsWith(dir));
});

test("a workspace with no marker keeps the canonical spelling", () => {
  const dir = workspace();
  assert.equal(resolveSocketPath({}, dir), join(dir, ".onlyne", "run", "s"));
});

test("a marker that is empty, relative, or unreadable falls through silently", () => {
  const empty = workspace("");
  assert.equal(resolveSocketPath({}, empty), join(empty, SOCKET_RELATIVE_PATH));

  const relative = workspace("run/s");
  assert.equal(resolveSocketPath({}, relative), join(relative, SOCKET_RELATIVE_PATH));

  // A `socket` leaf holding a directory makes the read itself fail.
  const unreadable = workspace();
  rmSync(join(unreadable, SOCKET_MARKER_RELATIVE_PATH), { force: true });
  mkdirSync(join(unreadable, SOCKET_MARKER_RELATIVE_PATH));
  assert.equal(resolveSocketPath({}, unreadable), join(unreadable, SOCKET_RELATIVE_PATH));

  // A workspace with no `run/` at all still names one canonical path.
  const bare = mkdtempSync(join(tmpdir(), "pi-onlyne-socket-"));
  cleanups.push(() => rmSync(bare, { recursive: true, force: true }));
  assert.equal(resolveSocketPath({}, bare), join(bare, SOCKET_RELATIVE_PATH));
});
