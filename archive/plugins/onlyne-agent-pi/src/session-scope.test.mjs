// The rule that keeps a pi subagent from settling its parent's task, and the
// sentence a foreign caller reads. The live defect these pin: a `task` subagent
// runs a sidechain in the same pi process with the same inherited environment,
// so it saw the parent's `ONLYNE_SESSION_ID` and its `onlyne_complete` settled
// the parent's task (librarian cfa73386, 2026-08-11).
import assert from "node:assert/strict";
import test from "node:test";
import {
  declaredSubagent,
  foreignSessionRefusal,
  isSessionReplacement,
  mountsSession,
  ownsSession,
  sessionIdOf,
} from "./session-scope.mjs";

const PARENT = "a379b47d-65b7-49c3-ad4b-b978cdf5a5d3";
const SIDECHAIN = "4f1cb760-c254-4ea1-9d55-6f2a1c8bb0e1";

test("the session id is read off the manager, and an unreadable one reads as unknown", () => {
  assert.equal(sessionIdOf({ getSessionId: () => PARENT }), PARENT);
  assert.equal(sessionIdOf({}), null, "no accessor, no id");
  assert.equal(sessionIdOf(undefined), null);
  assert.equal(sessionIdOf({ getSessionId: () => "" }), null, "an empty id names nothing");
  assert.equal(sessionIdOf({ getSessionId: () => 7 }), null, "a non-string id is not one");
  assert.equal(
    sessionIdOf({
      getSessionId: () => {
        throw new Error("stale context");
      },
    }),
    null,
    "a context that throws is one this plugin cannot read",
  );
});

test("only the session the client named mounts, and an unknown id does not stop the mount", () => {
  assert.equal(mountsSession(PARENT, PARENT), true, "the spawned session is the one");
  assert.equal(mountsSession(PARENT, SIDECHAIN), false, "a subagent sidechain is not");
  assert.equal(
    mountsSession(PARENT, null),
    true,
    "an older pi that cannot say which session this is keeps the env-bound mount",
  );
});

test("a call is the bound session's unless pi names another session", () => {
  assert.equal(ownsSession(PARENT, PARENT), true);
  assert.equal(ownsSession(PARENT, SIDECHAIN), false, "the sidechain must not settle the parent");
  assert.equal(ownsSession(PARENT, null), true, "a caller this pi cannot identify is allowed");
  assert.equal(
    ownsSession(null, SIDECHAIN),
    false,
    "a runner with no bound session has no session whose calls are its own",
  );
});

test("the refusal names both sessions and why the call is refused", () => {
  const sentence = foreignSessionRefusal(PARENT, SIDECHAIN);
  assert.ok(sentence.includes(SIDECHAIN), sentence);
  assert.ok(sentence.includes(PARENT), sentence);
  assert.ok(sentence.includes("subagent"), sentence);
});

test("a host that declares the session a subagent is read before any session id", () => {
  assert.deepEqual(
    declaredSubagent({ agent: { kind: "sub", id: "0-Explore", name: "explore", depth: 1, parentId: "Main" } }),
    { id: "0-Explore", parentId: "Main" },
  );
  assert.deepEqual(
    declaredSubagent({ agent: { kind: "sub", id: "tan-1", name: "sub", depth: 0 } }),
    { id: "tan-1", parentId: null },
    "a /tan clone is a subagent at depth 0, which is why depth is not the field read",
  );
  assert.deepEqual(
    declaredSubagent({ agent: { kind: "sub" } }),
    { id: null, parentId: null },
    "a declaration with no ids still refuses; the caller names what it can",
  );
  assert.equal(declaredSubagent({ agent: { kind: "main", id: "Main", name: "main", depth: 0 } }), null);
  assert.equal(declaredSubagent({ agent: "sub" }), null, "a non-object declaration declares nothing");
  assert.equal(declaredSubagent({ agent: null }), null);
  assert.equal(declaredSubagent({}), null, "a host that declares no agent identity is answered with none");
  assert.equal(declaredSubagent(undefined), null);
});

test("only the transitions a host calls a replacement read as one", () => {
  assert.equal(isSessionReplacement({ reason: "new" }), true);
  assert.equal(isSessionReplacement({ reason: "resume" }), true);
  assert.equal(isSessionReplacement({ reason: "fork" }), true);
  assert.equal(isSessionReplacement({ reason: "startup" }), false, "a second session beside the live one");
  assert.equal(isSessionReplacement({ reason: "reload" }), false, "the same session, reopened");
  assert.equal(isSessionReplacement({}), false, "a host that names no reason names no replacement");
  assert.equal(isSessionReplacement(undefined), false);
});
