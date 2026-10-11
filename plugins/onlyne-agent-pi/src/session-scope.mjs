// One question, asked twice: is the session in front of us the one the client
// spawned this process for?
//
// A pi process runs more than one session. The client spawns pi with
// `--session-id <the delivery's session>` and injects `ONLYNE_*` into the
// process environment, and pi then runs other sessions inside that same process
// with the same environment: a `task` subagent's sidechain
// (`@tintinweb/pi-subagents` re-binds every extension for the child,
// `session.bindExtensions`), and `/new`. Each of those loads this extension and
// sees the parent's `ONLYNE_SESSION_ID`, so a rule that read the environment
// alone would mount a second connection for the parent's session and let a
// subagent's `onlyne_complete` settle the parent's task — which is exactly what
// happened live (librarian task cfa73386, 2026-08-11: a writer subagent settled
// its parent at 05:15:47 while the parent kept writing).
//
// So the rule is the session's own id, read off the pid pi gave the handler, and
// the environment is only the expectation it is measured against. The two
// readings are separate on purpose: `mountsSession` is the door (does this
// session get a connection and the tools at all), `ownsSession` is the guard
// behind it (may this caller's tool call move the task), and a pi that cannot
// report a session id at all is the one case where both answer yes — refusing
// every call on an older pi would break the ordinary session to prevent nothing,
// because the env-bound mount is still the only one that connected.

/**
 * The id of the session a pi extension context belongs to.
 *
 * `null` means this pi cannot tell — no `sessionManager`, or one without
 * `getSessionId` — which is the honest reading of an unknown, and both rules
 * below treat it as "allowed" for the reason in the header.
 *
 * @param {{ getSessionId?: () => unknown } | null | undefined} sessionManager
 * @returns {string | null}
 */
export function sessionIdOf(sessionManager) {
  try {
    const id = sessionManager?.getSessionId?.();
    return typeof id === "string" && id.length > 0 ? id : null;
  } catch {
    // A context that throws is a context this plugin cannot read; the caller
    // gets the same answer as one that is absent.
    return null;
  }
}

/**
 * Whether the session pi is starting is the one the client spawned this process
 * for, and therefore whether it may mount.
 *
 * @param {string} identitySessionId `ONLYNE_SESSION_ID`, the client's own word
 * @param {string | null} startingSessionId the session's own id, or `null`
 * @returns {boolean}
 */
export function mountsSession(identitySessionId, startingSessionId) {
  return startingSessionId === null || startingSessionId === identitySessionId;
}

/**
 * Whether a call comes from the session that mounted this plugin.
 *
 * The guard behind the tools and the `/onlyne` command: a sidechain that reached
 * a tool through its parent's registration must not settle the parent's task,
 * and the sentence it earns says so rather than reporting an outcome nobody
 * asked this session for.
 *
 * @param {string | null} boundSessionId the session that mounted, or `null`
 *   before one has
 * @param {string | null} callingSessionId the session the call came from
 * @returns {boolean}
 */
export function ownsSession(boundSessionId, callingSessionId) {
  if (callingSessionId === null) return true;
  return boundSessionId !== null && callingSessionId === boundSessionId;
}

/**
 * The one sentence a call from a foreign session earns, for a tool error and for
 * a command notice alike.
 *
 * @param {string | null} boundSessionId
 * @param {string | null} callingSessionId
 * @returns {string}
 */
export function foreignSessionRefusal(boundSessionId, callingSessionId) {
  return (
    `onlyne: session ${callingSessionId} did not mount these tools; they belong to onlyne ` +
    `session ${boundSessionId}, and a subagent or another session in the same pi process ` +
    `must not settle this task`
  );
}
