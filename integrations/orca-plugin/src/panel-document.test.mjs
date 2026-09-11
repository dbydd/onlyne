// The panel document is the board's only channel into the panel (see the header
// of src/panel-document.mjs for the Orca surfaces behind that). These tests pin
// the parts a supervisor would notice: what the document shows, that it escapes
// what the CLIs answer, that it stays inert, when it is rewritten, and that its
// own script keeps the ages honest.

import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import vm from "node:vm";

import { collectBoard } from "./board.mjs";
import { summaryLine } from "./render.mjs";
import {
  JSON_BLOCK_ID,
  PANEL_GENERATED_MARKER,
  PANEL_PLACEHOLDER_MARKER,
  PANEL_SCRIPT,
  boardPayload,
  createPanelPublisher,
  panelFingerprint,
  panelWriteTarget,
  renderPanelDocument
} from "./panel-document.mjs";
import { fakeOnlyne, fakeOrca, roleRow, sessionRow, tabRow } from "./testing.mjs";

const ROOT = "/srv/cluster";

async function boardFixture({
  tabs = [tabRow()],
  sessions = [sessionRow()],
  roles = [roleRow()],
  failures = {},
  roots = [ROOT],
  tabFailure = null,
  claims = null,
  unpublished = []
} = {}) {
  return collectBoard({
    orca: fakeOrca({ tabs, tabFailure }),
    onlyne: fakeOnlyne({ sessions: { [ROOT]: sessions }, roles: { [ROOT]: roles }, failures }),
    serverRoots: roots,
    readClaims: claims || unpublished.length ? () => ({ claims: claims ?? [], unpublished }) : undefined
  });
}

function embeddedPayload(html) {
  const block = html.match(
    new RegExp(`<script type="application/json" id="${JSON_BLOCK_ID}">([\\s\\S]*?)</script>`)
  );
  assert.ok(block, "the document must embed the snapshot");
  return JSON.parse(block[1].replace(/\\u003c/g, "<"));
}

test("the document shows the summary, the role groups and the stray tabs", async () => {
  const board = await boardFixture({
    tabs: [tabRow(), tabRow({ handle: "term_orphan", title: "Pi ready", connected: false })],
    sessions: [
      sessionRow({ task_id: "task-alpha", updated_at: "1789093578" }),
      sessionRow({ task_id: "task-orphan", public_lifecycle: "exited", projection: {} })
    ],
    roles: [roleRow({ name: "planner" }), roleRow({ name: "reviewer", state: "offline", sessions: 0 })]
  });
  const html = renderPanelDocument(board, { generatedAt: 1_789_000_000_000 });

  // Summary line, ids shortened the way the text board shortens them.
  assert.match(html, /1 roots · 2 roles · 2 tabs \(1 live\) · 2 sessions \(1 working\)/);
  assert.match(html, /<span class="role">planner<\/span>/);
  assert.match(html, /<span class="role">reviewer<\/span>/);
  assert.match(html, /online · 2 tasks · 1 live/);
  assert.match(html, /task-alp/);
  assert.match(html, /working\/running/);
  // The joined row carries its pane, the unjoined one says so instead.
  assert.match(html, /45e603f7:b6d067b6/);
  assert.match(html, /无 tab/);
  // The unmatched tab is still listed, with its own title and worktree.
  assert.match(html, /未 join 的 tab \(1\)/);
  assert.match(html, /Pi ready/);
});

test("the document says what the tab axis was scoped to", async () => {
  const board = await boardFixture({
    tabs: [tabRow(), tabRow({ handle: "term_other", worktreePath: "/repo/elsewhere" })],
    roles: [roleRow()],
    sessions: [sessionRow()],
    // The fixture's tabs live in `/repo/other-swarm`, so a root inside it scopes
    // the axis; the second tab stays outside and is only counted.
    roots: ["/repo/other-swarm/cluster"]
  });
  const html = renderPanelDocument(board, { generatedAt: 1 });

  assert.match(html, /1 hidden/);
  assert.match(html, /只列 <code>\/repo\/other-swarm<\/code> 里的 tab/);
  assert.match(html, /其余 1 个 tab 不计入/);

  // A root outside every worktree cannot scope anything; the board says so and
  // lists everything rather than rendering empty. (`hiddenTabs` stays in the
  // payload, so assert on the rendered summary, not on the whole document.)
  const outside = await boardFixture({ roots: ["/srv/elsewhere"] });
  const unscoped = renderPanelDocument(outside, { generatedAt: 1 });
  assert.match(unscoped, /pi 插件还没申报 pane/);
  assert.equal(summaryLine(outside).includes("hidden"), false);
  assert.equal(outside.summary.hiddenTabs, 0);
});

test("a configured workspace that published nothing is named in the note", async () => {
  const PANE = "45e603f7-0772-48aa-bcf6-832272747713:b6d067b6-9255-4f5c-a13f-24f194ea0560";

  // A claim arrived from one workspace, and another was configured but silent:
  // the board keeps working and names the silent one, so a typo in a
  // hand-written `piWorkspaces` list cannot hide as a swarm that has not booted.
  const partial = await boardFixture({
    tabs: [tabRow({ paneKey: PANE, worktreePath: "/repo/other-swarm" })],
    roots: ["/repo/other-swarm/cluster"],
    claims: [{ paneKey: PANE, workspace: "/srv/swarm/planner" }],
    unpublished: ["/srv/swarm/builder"]
  });
  const scoped = renderPanelDocument(partial, { generatedAt: 1 });
  assert.match(scoped, /只列 pi 插件申报的 1 个 pane/);
  assert.match(scoped, /另有 1 个 workspace 没读到 pane 申报：<code>\/srv\/swarm\/builder<\/code>/);

  // Nothing published at all, and no worktree to fall back on: the note is the
  // only thing standing between the operator and "the filter is broken".
  const silent = await boardFixture({ roots: ["/srv/elsewhere"], unpublished: ["/srv/swarm/planner"] });
  const unscoped = renderPanelDocument(silent, { generatedAt: 1 });
  assert.match(unscoped, /配置的 1 个 workspace 都没读到 pane 申报（<code>\/srv\/swarm\/planner<\/code>）/);
  assert.match(unscoped, /列出全部 tab/);

  // Publishing is a change the panel must show even when no count moves: the
  // note mentions the workspace, so the fingerprint has to follow it.
  const before = await boardFixture({ roots: ["/srv/elsewhere"] });
  assert.equal(summaryLine(before), summaryLine(silent), "the counts are identical");
  assert.notEqual(panelFingerprint(before), panelFingerprint(silent));
});

test("the session age uses the epoch seconds the admin rows carry", async () => {
  // Measured 2026-09-11: `terminal list` gives epoch milliseconds, the admin
  // `sessions` row gives epoch *seconds* as a string, which Date.parse reads as
  // NaN — an unjoined task row must still show a real age.
  const board = await boardFixture({ tabs: [], sessions: [sessionRow({ updated_at: "1789093578" })] });
  const html = renderPanelDocument(board, { generatedAt: 1_789_093_600_000 });

  assert.match(html, /data-ts="1789093578000"/);
  assert.equal(/<td class="age-cell"><span class="age">—<\/span>/.test(html), false);
});

test("the embedded snapshot is the payload the debug command writes", async () => {
  const board = await boardFixture();
  const generatedAt = 1_789_000_000_000;
  const html = renderPanelDocument(board, { generatedAt });

  assert.deepEqual(embeddedPayload(html), boardPayload(board, { generatedAt }));
  const payload = embeddedPayload(html);
  assert.equal(payload.rows.length, board.rows.length);
  assert.equal(payload.summary.tabs, 1);
  assert.equal(payload.errors.length, 0);
});

test("everything the CLIs answer is escaped", async () => {
  const hostile = '</td><script>alert(1)</script><img src=x onerror="alert(2)">';
  const board = await boardFixture({
    tabs: [tabRow({ title: `onlyne:${hostile}` }), tabRow({ handle: "term_b", title: hostile })],
    sessions: [sessionRow({ role: hostile })],
    roles: [roleRow({ name: hostile })],
    failures: { [ROOT]: { ok: false, code: hostile, message: hostile } }
  });
  const html = renderPanelDocument(board, { generatedAt: 1 });

  assert.equal(/<img/.test(html), false, "a title must never become markup");
  assert.equal(/<script>alert/.test(html), false);
  assert.match(html, /&lt;script&gt;alert\(1\)&lt;\/script&gt;/);
  assert.match(html, /&lt;\/td&gt;/);
});

test("the document stays a document: no fetch, no bridge, no storage", async () => {
  const html = renderPanelDocument(await boardFixture(), { generatedAt: 1 });

  assert.equal(/fetch\(|XMLHttpRequest|WebSocket|localStorage|postMessage/.test(html), false);
  assert.equal(/<iframe/.test(html), false);
  assert.equal(/https?:\/\//.test(html), false);
});

test("an empty board renders the note, and the placeholder says what it is", async () => {
  const empty = renderPanelDocument(null);
  assert.match(empty, /serverRoots/);
  assert.equal(/id="stamp"/.test(empty), false, "no snapshot means no age stamp");

  const placeholder = renderPanelDocument(null, { generatedAt: null, placeholder: true });
  assert.match(placeholder, new RegExp(PANEL_PLACEHOLDER_MARKER));
  assert.equal(placeholder.includes(PANEL_GENERATED_MARKER), false);
});

test("the fingerprint follows structure and state, not the clock", async () => {
  const board = await boardFixture({ sessions: [sessionRow({ updated_at: "1789093578" })] });
  const later = await boardFixture({ sessions: [sessionRow({ updated_at: "1789093999" })] });
  assert.equal(panelFingerprint(board), panelFingerprint(later), "timestamps must not rewrite the panel");

  const appeared = await boardFixture({
    sessions: [sessionRow(), sessionRow({ task_id: "task-beta", session_id: "sess-beta" })]
  });
  assert.notEqual(panelFingerprint(board), panelFingerprint(appeared));

  const working = await boardFixture();
  const exited = await boardFixture({
    sessions: [sessionRow({ public_lifecycle: "exited", projection: { lifecycle: "exited", outcome: "done" } })]
  });
  assert.notEqual(panelFingerprint(working), panelFingerprint(exited));

  const broken = await boardFixture({
    failures: { [ROOT]: { ok: false, code: "cli_error", message: "no socket" } }
  });
  assert.notEqual(panelFingerprint(board), panelFingerprint(broken));

  // A scope that changes source re-renders the note even when the counts and
  // the rows it filters to are identical, so the fingerprint must follow it.
  const unclaimed = await boardFixture();
  const claimed = await boardFixture({ claims: [{ paneKey: unclaimed.tabs[0].paneKey }] });
  assert.equal(claimed.scope.source, "adapter");
  assert.deepEqual(claimed.summary, unclaimed.summary, "the two boards render the same counts");
  assert.notEqual(panelFingerprint(unclaimed), panelFingerprint(claimed));
});

test("only a mutable dev tree is a write target", () => {
  assert.equal(panelWriteTarget({ rootDir: "/repo/integrations/orca-plugin" }), "/repo/integrations/orca-plugin/panel.html");
  assert.equal(panelWriteTarget({ rootDir: `/plugins/onlyne/${"a".repeat(64)}` }), null);
  assert.equal(panelWriteTarget({ rootDir: "" }), null);
  assert.equal(panelWriteTarget({}), null);
  assert.equal(panelWriteTarget({ rootDir: "/dev/tree", entry: "../outside.html" }), null);
});

test("the publisher writes once per change, heals a placeholder, and never touches an install", async () => {
  const root = mkdtempSync(join(tmpdir(), "onlyne-panel-"));
  const logs = [];
  const publisher = createPanelPublisher({ rootDir: root, log: (line) => logs.push(line) });
  const target = join(root, "panel.html");

  const board = await boardFixture();
  const first = publisher.publish(board);
  assert.equal(first.written, true);
  assert.equal(first.reason, "changed");
  assert.ok(first.bytes > 0);
  const firstDocument = readFileSync(target, "utf8");
  assert.match(firstDocument, new RegExp(PANEL_GENERATED_MARKER));
  const payloadAtPublish = embeddedPayload(firstDocument);
  assert.equal(payloadAtPublish.panel.target, target, "the snapshot names the file it lives in");
  // The worker hands `debug-board` the same `panel` object, so the dump and the
  // document carry the identical payload apart from the write instant.
  assert.deepEqual(
    { ...payloadAtPublish, generatedAt: 0 },
    { ...boardPayload(board, { panel: { target } }), generatedAt: 0 },
    "the panel document embeds the debug-board payload"
  );
  assert.equal(readdirSync(root).length, 1, "the atomic write leaves no temp file");

  const second = publisher.publish(board);
  assert.equal(second.written, false);
  assert.equal(second.reason, "unchanged");
  assert.equal(readFileSync(target, "utf8"), firstDocument);

  // A `git checkout` of the committed placeholder is healed on the next scan.
  writeFileSync(target, renderPanelDocument(null, { generatedAt: null, placeholder: true }), "utf8");
  const healed = publisher.publish(board);
  assert.equal(healed.written, true);
  assert.equal(healed.reason, "placeholder");

  const changed = await boardFixture({
    sessions: [sessionRow(), sessionRow({ task_id: "task-beta", session_id: "sess-beta" })]
  });
  assert.equal(publisher.publish(changed).written, true);

  // A content-addressed install is verified per panel load: never write there.
  const installParent = mkdtempSync(join(tmpdir(), "onlyne-install-"));
  const installed = join(installParent, "b".repeat(64));
  mkdirSync(installed);
  const installedPublisher = createPanelPublisher({
    rootDir: installed,
    log: (line) => logs.push(line)
  });
  const refused = installedPublisher.publish(board);
  assert.equal(refused.written, false);
  assert.equal(refused.reason, "installed-tree");
  assert.equal(refused.path, null);
  assert.deepEqual(readdirSync(installed), []);
});

test("a failed write is reported, never thrown", async () => {
  const logs = [];
  const publisher = createPanelPublisher({
    rootDir: mkdtempSync(join(tmpdir(), "onlyne-panel-")),
    log: (line) => logs.push(line),
    fs: {
      mkdirSync: () => {},
      readFileSync: () => {
        throw new Error("no file");
      },
      writeFileSync: () => {
        throw new Error("read-only file system");
      },
      renameSync: () => {}
    }
  });
  const result = publisher.publish(await boardFixture());

  assert.equal(result.written, false);
  assert.equal(result.reason, "write-failed");
  assert.match(logs.join("\n"), /read-only file system/);
});

/** The document's own script, in a vm with the DOM surface it actually uses. */
function runPanelScript({ ages, stampTs, now }) {
  const nodes = ages.map((ts) => ({
    textContent: "—",
    attributes: { "data-ts": String(ts) },
    getAttribute(name) {
      return this.attributes[name] ?? null;
    }
  }));
  const stamp = stampTs === null ? null : { getAttribute: () => String(stampTs) };
  const body = { className: "" };
  const ticks = [];
  const sandbox = {
    document: {
      querySelectorAll: (selector) => (selector === "[data-ago]" ? nodes : []),
      getElementById: (id) => (id === "stamp" ? stamp : null),
      body
    },
    Date: { now: () => now.value },
    setInterval: (fn, ms) => {
      ticks.push({ fn, ms });
      return 1;
    },
    isFinite,
    Math,
    Number
  };
  vm.runInNewContext(PANEL_SCRIPT, sandbox, { filename: "panel-script" });
  return { nodes, body, ticks };
}

test("the panel script ticks ages from the embedded stamps", () => {
  const now = { value: 1_000_000 };
  const { nodes, ticks } = runPanelScript({ ages: [988_000], stampTs: 1_000_000, now });

  assert.equal(nodes[0].textContent, "12s");
  assert.equal(ticks[0].ms, 1000);
  // Ten seconds later the label moved, and the snapshot is still fresh enough
  // to be read as an exact age.
  now.value += 10_000;
  ticks[0].fn();
  assert.equal(nodes[0].textContent, "22s");
});

test("a snapshot older than the cadence is marked stale, with ages still exact", () => {
  const now = { value: 1_000_000 };
  const { nodes, body } = runPanelScript({ ages: [998_000], stampTs: 980_000, now });

  assert.equal(nodes[0].textContent, "2s");
  assert.equal(body.className, "stale");
});

test("a document with no snapshot stamp does not throw", () => {
  assert.doesNotThrow(() => runPanelScript({ ages: [], stampTs: null, now: { value: 1 } }));
});

test("the committed placeholder is the packaged-install degradation", () => {
  const html = readFileSync(new URL("../panel.html", import.meta.url), "utf8");

  assert.match(html, new RegExp(PANEL_PLACEHOLDER_MARKER));
  assert.equal(html.includes(PANEL_GENERATED_MARKER), false);
  assert.equal(/data-ts="/.test(html), false, "the placeholder shows no ages");
});