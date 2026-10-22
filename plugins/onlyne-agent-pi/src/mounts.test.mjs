// The routing decisions across the mounts one process holds: which mount a
// target names, which one a reply belongs to, and which role a handoff
// continues into. Each rule answers only where the answer is unique, and every
// other case is a refusal that names what it could have chosen — a guess here
// delivers the message to the wrong cluster's ledger
// (`docs/v2.12-ONE-SESSION-TWO-CLUSTERS.md` §3).
//
// A mount is `{ agent, cluster, role }` and the resolver reads only the agent's
// task table, so a fake mount is the whole fixture and no client is needed.
import assert from "node:assert/strict";
import test from "node:test";

import { handoffRole, resolveTarget, resolveTaskMount } from "./mounts.mjs";

/** One mount; `active` is how many tasks its agent holds. */
function mount(cluster, role, active = 0) {
  return {
    agent: {
      activeTasks: () => Array.from({ length: active }, (_, index) => ({ taskId: `task-${index}` })),
    },
    cluster,
    role,
  };
}

test("cluster:role picks that mount and strips the prefix", () => {
  const mounts = [mount("chamber", "planner", 1), mount("workshop", "builder", 2)];
  assert.deepEqual(resolveTarget(mounts, "workshop:builder"), { mount: mounts[1], to: "builder" });
  // Whitespace around either side belongs to the operator, not to a name.
  assert.deepEqual(resolveTarget(mounts, "  workshop : builder  "), { mount: mounts[1], to: "builder" });
  // The prefix answers even while both mounts hold work: it is the one spelling
  // that names a layer outright.
  assert.equal(resolveTarget(mounts, "chamber:builder").mount, mounts[0]);
});

test("an unknown cluster is refused with every mounted cluster listed", () => {
  const mounts = [mount("chamber", "lazuli"), mount("workshop", "planner", 1)];
  assert.throws(
    () => resolveTarget(mounts, "nowhere:builder"),
    /onlyne: no mount serves cluster "nowhere" \(mounted: chamber:lazuli, workshop:planner\)/,
  );
});

test("a bare role reaches the only mount, or the one holding the work", () => {
  const only = [mount(null, "lazuli")];
  assert.deepEqual(resolveTarget(only, " builder "), { mount: only[0], to: "builder" });

  // A reply belongs to the layer that handed the task over.
  const holding = [mount("chamber", "lazuli"), mount("workshop", "lazuli", 1)];
  assert.equal(resolveTarget(holding, "builder").mount, holding[1]);
});

test("a bare role this process cannot place is refused, never guessed", () => {
  const idle = [mount("chamber", "lazuli"), mount("workshop", "lazuli")];
  assert.throws(
    () => resolveTarget(idle, "builder"),
    /"builder" names no cluster and this session holds 2 mounts \(chamber:lazuli, workshop:lazuli\); write cluster:role/,
  );

  const busy = [mount("chamber", "lazuli", 1), mount("workshop", "lazuli", 1)];
  assert.throws(
    () => resolveTarget(busy, "builder"),
    /more than one mount has work in flight \(chamber:lazuli, workshop:lazuli\); write cluster:role/,
  );
});

test("a reply goes to the cluster it names, over any work in flight", () => {
  const mounts = [mount("chamber", "lazuli", 1), mount("workshop", "lazuli", 1)];
  assert.equal(resolveTaskMount(mounts, "workshop"), mounts[1]);
});

test("without a cluster the one mount holding a task answers", () => {
  const mounts = [mount("chamber", "lazuli"), mount("workshop", "lazuli", 2)];
  assert.equal(resolveTaskMount(mounts), mounts[1]);
});

test("a reply the process cannot place is refused rather than sent to the first mount", () => {
  const idle = [mount("chamber", "lazuli"), mount("workshop", "lazuli")];
  assert.throws(
    () => resolveTaskMount(idle),
    /this session holds 2 mounts \(chamber:lazuli, workshop:lazuli\) and no single task; name the cluster/,
  );

  const busy = [mount("chamber", "lazuli", 1), mount("workshop", "lazuli", 1)];
  assert.throws(
    () => resolveTaskMount(busy),
    /this session holds 2 mounts \(chamber:lazuli, workshop:lazuli\) and no single task; name the cluster/,
  );
});

test("a handoff continues in the task's own cluster", () => {
  const workshop = mount("workshop", "lazuli");
  assert.equal(handoffRole("builder", workshop), "builder");
  assert.equal(handoffRole(" builder ", workshop), "builder");
  assert.equal(handoffRole("workshop:builder", workshop), "builder");
  // A mount that declares no cluster can still verify the bare name.
  assert.equal(handoffRole("builder", mount(null, "lazuli")), "builder");
});

test("a handoff across layers or past an undeclared cluster is refused", () => {
  const workshop = mount("workshop", "lazuli");
  assert.throws(
    () => handoffRole("chamber:builder", workshop),
    /this task belongs to cluster "workshop"; "chamber" is a different layer/,
  );
  assert.throws(
    () => handoffRole("workshop:builder", mount(null, "lazuli")),
    /this mount declares no cluster, so "workshop:builder" cannot be verified; write the role alone/,
  );
});
