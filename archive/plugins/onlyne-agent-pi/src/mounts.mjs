// Routing across the mounts one process holds.
//
// A resident plugin may serve the same role in more than one cluster
// (`docs/v2.12-ONE-SESSION-TWO-CLUSTERS.md`). Every mount keeps its own client
// link and its own task table, so "where does this message go" is a decision
// over a list, and every answer that cannot be given uniquely is a refusal
// naming what it could have chosen. This module holds that decision and nothing
// else: no socket, no pi surface, no timers.
//
// A mount is `{ agent, cluster, role }`; only `agent.activeTasks()` is read
// from the agent, which is what makes the rule testable without a connection.

/** How one mount is named in a refusal or on a delivery card. */
export function mountLabel(mount) {
  return mount.cluster ? `${mount.cluster}:${mount.role}` : mount.role;
}

/** Every mount, for a refusal that has to list what it could have chosen. */
export function mountLabels(mounts) {
  return mounts.map(mountLabel).join(", ");
}

/** The one mount with a task in flight, or `null` when none or several. */
export function soleActiveMount(mounts) {
  const active = mounts.filter((mount) => mount.agent.activeTasks().length > 0);
  return active.length === 1 ? active[0] : null;
}

/**
 * Resolve a `to` into a mount and a bare role name.
 *
 * `cluster:role` names both. A bare role goes to the mount that owns the work
 * in flight — a reply belongs to the layer that handed the task over — or to
 * the only mount this process holds. Anything else is refused with every mount
 * listed: the adapter socket answers `query_roles` for the client's own role
 * only, so no roster exists to prove which cluster holds a bare name, and a
 * guess here delivers a message to the wrong layer.
 *
 * @param {Array<{ agent: { activeTasks(): unknown[] }, cluster: string | null, role: string }>} mounts
 * @param {string} target
 * @returns {{ mount: any, to: string }}
 */
export function resolveTarget(mounts, target) {
  const text = typeof target === "string" ? target : "";
  const split = text.indexOf(":");
  if (split > 0) {
    const cluster = text.slice(0, split).trim();
    const role = text.slice(split + 1).trim();
    if (!role) throw new Error(`onlyne: "${text}" names a cluster but no role`);
    const mount = mounts.find((candidate) => candidate.cluster === cluster);
    if (!mount) {
      throw new Error(`onlyne: no mount serves cluster "${cluster}" (mounted: ${mountLabels(mounts)})`);
    }
    return { mount, to: role };
  }
  const active = soleActiveMount(mounts);
  if (active) return { mount: active, to: text.trim() };
  if (mounts.length === 1) return { mount: mounts[0], to: text.trim() };
  const inFlight = mounts.filter((mount) => mount.agent.activeTasks().length > 0).length;
  if (inFlight === 0) {
    throw new Error(
      `onlyne: "${text}" names no cluster and this session holds ${mounts.length} mounts (${mountLabels(mounts)}); write cluster:role`
    );
  }
  throw new Error(`onlyne: more than one mount has work in flight (${mountLabels(mounts)}); write cluster:role`);
}

/**
 * Resolve the mount a reply belongs to.
 *
 * `cluster` is the model's own disambiguation when two mounts hold work at
 * once; without it the single mount holding an open task answers, and anything
 * else is refused. A reply never leaves on "the first mount": two clusters in
 * one process means two ledgers, and settling the wrong one leaves the real
 * task open and the other falsely done.
 *
 * @param {Array<{ agent: { activeTasks(): unknown[] }, cluster: string | null, role: string }>} mounts
 * @param {string} [cluster]
 * @returns {any}
 */
export function resolveTaskMount(mounts, cluster) {
  if (typeof cluster === "string" && cluster.trim() !== "") {
    const name = cluster.trim();
    const mount = mounts.find((candidate) => candidate.cluster === name);
    if (!mount) {
      throw new Error(`onlyne: no mount serves cluster "${name}" (mounted: ${mountLabels(mounts)})`);
    }
    return mount;
  }
  const active = soleActiveMount(mounts);
  if (active) return active;
  if (mounts.length === 1) return mounts[0];
  throw new Error(
    `onlyne: this session holds ${mounts.length} mounts (${mountLabels(mounts)}) and no single task; name the cluster`
  );
}

/**
 * The role name a handoff continues into.
 *
 * A handoff stays inside the task's own cluster: the mount that holds the task
 * is also the ledger the child is minted in, so moving work across layers is a
 * delivery rather than a continuation.
 *
 * @param {string} target
 * @param {{ cluster: string | null }} mount
 * @returns {string}
 */
export function handoffRole(target, mount) {
  const text = typeof target === "string" ? target : "";
  const split = text.indexOf(":");
  if (split <= 0) return text.trim();
  const cluster = text.slice(0, split).trim();
  if (mount.cluster === null) {
    throw new Error(
      `onlyne: this mount declares no cluster, so "${text}" cannot be verified; write the role alone`
    );
  }
  if (cluster !== mount.cluster) {
    throw new Error(
      `onlyne: this task belongs to cluster "${mount.cluster}"; "${cluster}" is a different layer, and a handoff continues the task in its own cluster`
    );
  }
  return text.slice(split + 1).trim();
}
