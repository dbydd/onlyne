// The panel is a static sandboxed document; this runs its inline script in a
// vm with a minimal DOM + postMessage host so the bridge contract (request
// shape, requestId echo, error and timeout rendering) is proven without a
// browser.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import vm from "node:vm";

const panelPath = join(dirname(fileURLToPath(import.meta.url)), "..", "panel.html");
const panelHtml = readFileSync(panelPath, "utf8");

function makeElement({ id = null, attributes = {} } = {}) {
  const element = {
    id,
    attributes,
    textContent: "",
    innerHTML: "",
    listeners: {},
    addEventListener(type, handler) {
      (element.listeners[type] ??= []).push(handler);
    },
    getAttribute(name) {
      return attributes[name] ?? null;
    },
    click() {
      for (const handler of element.listeners.click ?? []) handler({ target: element });
    },
  };
  return element;
}

function boot() {
  const script = panelHtml.slice(
    panelHtml.lastIndexOf("<script>") + "<script>".length,
    panelHtml.lastIndexOf("</script>")
  );
  const ids = new Map(["workspace", "error", "status", "read-context"].map((id) => [id, makeElement({ id })]));
  const hints = [...panelHtml.matchAll(/data-hint="([^"]+)"/g)].map((match) =>
    makeElement({ attributes: { "data-hint": match[1] } })
  );
  const sent = [];
  const messageHandlers = [];
  const timeouts = [];
  const window = {
    parent: null,
    addEventListener(type, handler) {
      if (type === "message") messageHandlers.push(handler);
    },
    removeEventListener(type, handler) {
      if (type !== "message") return;
      const index = messageHandlers.indexOf(handler);
      if (index >= 0) messageHandlers.splice(index, 1);
    },
    postMessage(message) {
      sent.push(message);
    },
  };
  window.parent = window;
  const document = {
    getElementById: (id) => ids.get(id) ?? null,
    querySelectorAll: (selector) => (selector === "button[data-hint]" ? hints : []),
  };
  const sandbox = {
    window,
    document,
    setTimeout: (handler, ms) => {
      timeouts.push({ handler, ms });
      return timeouts.length;
    },
  };
  vm.runInNewContext(script, sandbox, { filename: "panel.html" });
  return {
    ids,
    hints,
    sent,
    timeouts,
    lastRequest: () => sent.at(-1),
    reply(result) {
      const request = sent.at(-1);
      for (const handler of [...messageHandlers]) {
        handler({ data: { type: "orca-panel-action-result", requestId: request.requestId, ...result } });
      }
    },
    fireTimeouts() {
      for (const { handler } of timeouts.splice(0)) handler();
    },
  };
}

test("the panel asks the host for the focused workspace on load", () => {
  const panel = boot();
  assert.equal(panel.sent.length, 1);
  const request = panel.lastRequest();
  assert.equal(request.type, "orca-panel-action");
  assert.equal(request.action, "workspace.readContext");
  assert.equal(Object.keys(request.params).length, 0);
  assert.ok(request.requestId.length > 0);
});

/** The panel resolves its bridge promises in microtasks; let them run. */
function flush() {
  return new Promise((resolve) => setImmediate(resolve));
}

test("a workspace answer renders name, branch and tab count", async () => {
  const panel = boot();
  panel.reply({
    ok: true,
    value: { displayName: "role-planner", branch: "v1", terminals: [{ id: "term_1" }, { id: "term_2" }] },
  });
  await flush();
  assert.match(panel.ids.get("workspace").innerHTML, /role-planner/);
  assert.match(panel.ids.get("workspace").innerHTML, /v1/);
  assert.match(panel.ids.get("workspace").innerHTML, /tab 数.*2/s);
  assert.match(panel.ids.get("status").textContent, /已读取/);
});

test("a refusal surfaces the host error code", async () => {
  const panel = boot();
  panel.reply({ ok: false, errorCode: "consent_required", error: "user consent required" });
  await flush();
  assert.match(panel.ids.get("error").textContent, /consent_required/);
});

test("a silent host times out instead of hanging", async () => {
  const panel = boot();
  panel.fireTimeouts();
  await flush();
  assert.match(panel.ids.get("error").textContent, /workspace\.readContext 失败：timeout/);
});

test("hint buttons post a notification naming the command to run", async () => {
  const panel = boot();
  panel.reply({ ok: true, value: { displayName: "w", branch: "b", terminals: [] } });
  await flush();
  assert.ok(panel.hints.length >= 4);
  const button = panel.hints.find((element) => element.attributes["data-hint"].includes("推送映射看板"));
  button.click();
  const request = panel.lastRequest();
  assert.equal(request.action, "notifications.show");
  assert.match(request.params.body, /Onlyne Sessions: 推送映射看板/);
  assert.ok(request.params.body.length <= 1000);
  assert.ok(request.params.title.length <= 120);
  panel.reply({ ok: true, value: { delivered: true } });
  await flush();
  assert.match(panel.ids.get("status").textContent, /已发送/);
});
