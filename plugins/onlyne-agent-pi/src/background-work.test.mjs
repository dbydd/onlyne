import assert from "node:assert/strict";
import { test } from "node:test";

import { createBackgroundProbe } from "./background-work.mjs";

const RESPONSE_CHANNEL = "pi-background-tasks:response:v1";
const RESPONSE_SCHEMA = "pi-background-tasks.extension-response.v1";

class FakeEventBus {
  constructor(answer) {
    this.answer = answer;
    this.emits = [];
    this.listeners = new Map();
    this.drops = 0;
  }

  on(channel, handler) {
    const handlers = this.listeners.get(channel) ?? new Set();
    handlers.add(handler);
    this.listeners.set(channel, handlers);
    let active = true;
    return () => {
      if (!active) return;
      active = false;
      this.drops += 1;
      handlers.delete(handler);
    };
  }

  emit(channel, data) {
    this.emits.push({ channel, data });
    if (channel !== "pi-background-tasks:request:v1") return;
    this.answer?.(data, (frame) => {
      for (const handler of [...(this.listeners.get(RESPONSE_CHANNEL) ?? [])]) {
        handler(frame);
      }
    });
  }

  listenerCount() {
    return [...this.listeners.values()].reduce((total, handlers) => total + handlers.size, 0);
  }
}

function response(request, body) {
  return {
    schema_version: RESPONSE_SCHEMA,
    request_id: request.request_id,
    ...body,
  };
}

function probeFor(answer, timeoutMs = 20) {
  const events = new FakeEventBus(answer);
  const probe = createBackgroundProbe({
    events,
    getToolNames: () => ["bg_run"],
    timeoutMs,
  });
  return { events, probe };
}

test("a session without a background-task tool makes no EventBus query", async () => {
  const events = new FakeEventBus(() => {
    assert.fail("an uninstalled background-task extension must not be queried");
  });
  const probe = createBackgroundProbe({
    events,
    getToolNames: () => ["read_file"],
  });

  assert.equal(await probe.running(), false);
  assert.deepEqual(events.emits, []);
  assert.equal(events.listenerCount(), 0);
});

test("a live background task answers true and drops the response listener", async () => {
  const { events, probe } = probeFor((request, respond) => {
    respond(response(request, { ok: true, result: { tasks: [{ status: "running" }] } }));
  });

  assert.equal(await probe.running(), true);
  assert.equal(events.emits.length, 1);
  assert.equal(events.listenerCount(), 0);
  assert.equal(events.drops, 1);
});

test("a terminal background task list answers false and drops the response listener", async () => {
  const { events, probe } = probeFor((request, respond) => {
    respond(response(request, {
      ok: true,
      result: { tasks: [{ status: "completed" }, { status: "failed" }] },
    }));
  });

  assert.equal(await probe.running(), false);
  assert.equal(events.listenerCount(), 0);
  assert.equal(events.drops, 1);
});

test("a refused status query answers false and drops the response listener", async () => {
  const { events, probe } = probeFor((request, respond) => {
    respond(response(request, { ok: false, error: "status unavailable" }));
  });

  assert.equal(await probe.running(), false);
  assert.equal(events.listenerCount(), 0);
  assert.equal(events.drops, 1);
});

test("a malformed status answer reads false and drops the response listener", async () => {
  const { events, probe } = probeFor((request, respond) => {
    respond(response(request, { ok: true, result: { tasks: "running" } }));
  });

  assert.equal(await probe.running(), false);
  assert.equal(events.listenerCount(), 0);
  assert.equal(events.drops, 1);
});

test("a background-task service that never answers reads false and drops the response listener", async () => {
  const { events, probe } = probeFor(null, 10);

  assert.equal(await probe.running(), false);
  assert.equal(events.emits.length, 1);
  assert.equal(events.listenerCount(), 0);
  assert.equal(events.drops, 1);
});
