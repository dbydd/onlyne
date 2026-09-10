import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { parseSwarmHeader, renderSwarmHeader, readSwarmEnabled, stripJsonc, readSwarmModel, treePathForWorkspace } from '../dist/swarm.js';

function schedulerWire(taskId, payload, from = '.', transfer = '', attempt = 1) {
  return `---swarm\ntask_id: ${taskId}\nfrom: ${from}\ntransfer_send_to: ${transfer}\nattempt: ${attempt}\ndelivery: scheduler\n---\n${payload}\n`;
}

test('swarm header roundtrip', () => {
  const id = '550e8400-e29b-41d1-a716-446655440000';
  const wire = renderSwarmHeader({ task_id: id, from: 'planner', transfer_send_to: '', attempt: 1 }, 'Review the incoming task and return a concise result.', 'do X\n');
  const m = parseSwarmHeader(wire);
  assert.ok(m);
  assert.equal(m.header.task_id, id);
  assert.equal(m.header.from, 'planner');
  assert.equal(m.header.transfer_send_to, '');
  assert.equal(m.delivery, '');
  assert.equal(m.payload, 'Review the incoming task and return a concise result.\n\ndo X\n');
});

test('non-swarm body is null', () => {
  assert.equal(parseSwarmHeader('hello'), null);
  assert.equal(parseSwarmHeader('---swarm\ntask_id: nope\n---\nx'), null);
});

test('legacy reply_to header is not a swarm message', () => {
  assert.equal(parseSwarmHeader('---swarm\ntask_id: 550e8400-e29b-41d1-a716-446655440000\nfrom: .\nreply_to: \nattempt: 1\n---\nx\n'), null);
});

test('swarm flag reads [swarm] enabled', () => {
  const dir = mkdtempSync(join(tmpdir(), 'swarm-'));
  try {
    const od = join(dir, '.onlyne');
    mkdirSync(od, { recursive: true });
    assert.equal(readSwarmEnabled(od), false);
    writeFileSync(join(od, 'config.toml'), '[workspace]\nname="x"\n\n[swarm]\nenabled = true\n');
    assert.equal(readSwarmEnabled(od), true);
    writeFileSync(join(od, 'config.toml'), '[workspace]\nname="x"\n\n[swarm]\nenabled = false\n');
    assert.equal(readSwarmEnabled(od), false);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test('swarm slot: consecutive hops on one workspace claim in order', async () => {
  // Regression for 875dff0d: second hop's session must claim ITS env task,
  // never the previous hop's done task, even when history lists old first.
  const { __swarmSlotForTest } = await import('../dist/swarm-slot.js');
  const pi = { sendMessage: () => {} };
  const slot = __swarmSlotForTest();
  const hop1 = '11111111-2222-4333-8444-555555555555';
  const hop2 = '22222222-3333-4444-8555-666666666666';
  const t2 = schedulerWire(hop2, 'Review this task, then complete it.');
  // catchUp scans inbounds; env match wins over newest-first fallback.
  assert.equal(slot.handle(pi, t2, hop2), 'claimed');
  assert.equal(slot.taskId(), hop2);
});

test('swarm slot: raw relay is rejected and scheduler delivery claims prose role', async () => {
  const { __swarmSlotForTest } = await import('../dist/swarm-slot.js');
  const seen = [];
  const pi = { sendMessage: (msg, opts) => { seen.push({ text: msg.content, deliverAs: opts?.deliverAs, triggerTurn: opts?.triggerTurn }); } };
  const slot = __swarmSlotForTest();
  const idA = '11111111-2222-4333-8444-555555555555';
  const rawRelay = `---swarm\ntask_id: ${idA}\nfrom: .\ntransfer_send_to: \nattempt: 1\n---\nROUND 7\n`;
  const taskA = schedulerWire(idA, 'Review the incoming task and return a concise result.');
  assert.equal(slot.handle(pi, rawRelay), 'not-swarm');
  assert.equal(slot.taskId(), undefined);
  assert.equal(slot.handle(pi, taskA), 'claimed');
  assert.equal(slot.taskId(), idA);
  assert.match(seen[0].text, /Review the incoming task/);
  // A claimed session never accepts another delivery.
  const taskB = schedulerWire('22222222-3333-4444-8555-666666666666', 'A child delivery.', 'a', idA);
  assert.equal(slot.handle(pi, taskB), 'not-swarm');
  assert.equal(slot.handle(pi, 'plain hello'), 'not-swarm');
  assert.ok(seen.every((m) => m.deliverAs === 'followUp'));
  assert.ok(seen.every((m) => m.triggerTurn === true), 'cold start needs triggerTurn');
  slot.clear();
  assert.equal(slot.taskId(), undefined);
});

test('swarm slot: env task preempts stale history claim', async () => {
  const { __swarmSlotForTest } = await import('../dist/swarm-slot.js');
  const seen = [];
  const pi = { sendMessage: (msg, opts) => { seen.push({ text: msg.content, deliverAs: opts?.deliverAs, triggerTurn: opts?.triggerTurn }); } };
  const slot = __swarmSlotForTest();
  const oldId = 'aaaaaaaa-2222-4333-8444-555555555555';
  const envId = 'bbbbbbbb-3333-4444-8555-666666666666';
  const stale = schedulerWire(oldId, 'Stale scheduler delivery.');
  const envTask = schedulerWire(envId, 'The real delivery.', '.', '', 1);
  assert.equal(slot.handle(pi, stale), 'claimed');
  assert.equal(slot.taskId(), oldId);
  // Env task arrives late: yields the stale claim, injects the real task.
  assert.equal(slot.handle(pi, envTask, envId), 'yielded');
  assert.equal(slot.taskId(), envId);
  assert.equal(seen.length, 2);
  // Third task while claimed: ignored.
  const other = schedulerWire('cccccccc-4444-4555-8666-777777777777', 'A different delivery.');
  assert.equal(slot.handle(pi, other, envId), 'not-swarm');
});

test('swarm slot: noteSpawned tracks spawned children', async () => {
  const { __swarmSlotForTest } = await import('../dist/swarm-slot.js');
  const pi = { sendMessage: () => {} };
  const slot = __swarmSlotForTest();
  const idA = '11111111-2222-4333-8444-555555555555';
  slot.handle(pi, schedulerWire(idA, 'A prose task.'));
  slot.noteSpawned('child-1');
  slot.noteSpawned('child-2');
  const t = slot.task();
  assert.deepEqual(t.sentChildIds, ['child-1', 'child-2']);
  assert.equal(t.transferSendTo, '');
  slot.clear();
  assert.equal(slot.taskId(), undefined);
});

test('stripJsonc drops comments and trailing commas', () => {
  const src = '{\n  // comment with "quote, and braces }\n  "a": "has // not a comment",\n  "b": [1,2,],\n}';
  const parsed = JSON.parse(stripJsonc(src));
  assert.deepEqual(parsed, { a: 'has // not a comment', b: [1, 2] });
});

test('readSwarmModel parses snapshot triple', () => {
  const dir = mkdtempSync(join(tmpdir(), 'pi-onlyne-model-'));
  writeFileSync(join(dir, 'swarm.workspace.jsonc'),
    '{\n  // model block\n  "name": "scout",\n  "model": { "provider": "axonhub", "model": "supercheap", "effort": "medium", },\n}');
  assert.deepEqual(readSwarmModel(dir), { provider: 'axonhub', model: 'supercheap', effort: 'medium' });
  writeFileSync(join(dir, 'swarm.workspace.jsonc'), '{"model": {"provider": "", "model": "", "effort": ""}}');
  assert.equal(readSwarmModel(dir), null);
  rmSync(dir, { recursive: true, force: true });
});

test('treePathForWorkspace maps .ws children and root', () => {
  assert.equal(treePathForWorkspace('/r/.ws/model'), 'model');
  assert.equal(treePathForWorkspace('/r/.ws/a/b'), 'a/b');
  assert.equal(treePathForWorkspace('/r'), '.');
  assert.equal(treePathForWorkspace('/r/.ws'), '.');
});

test('swarm prompt: defaults to built-in wrapper with task fields', async () => {
  const { resolveSwarmPrompt } = await import('../dist/swarm-prompt.js');
  const dir = mkdtempSync(join(tmpdir(), 'pi-onlyne-prompt-'));
  try {
    const out = resolveSwarmPrompt(dir, { task_id: 'id-1', from: 'a', transfer_send_to: '', attempt: 1 }, 'do X');
    assert.ok(out.includes('id-1'));
    assert.ok(out.includes('do X'));
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test('swarm prompt: disabled returns null (raw payload path)', async () => {
  const { resolveSwarmPrompt } = await import('../dist/swarm-prompt.js');
  const dir = mkdtempSync(join(tmpdir(), 'pi-onlyne-prompt-'));
  try {
    mkdirSync(join(dir, '.pi'), { recursive: true });
    writeFileSync(join(dir, '.pi', 'onlyne.json'), '{"swarm_prompt": false}');
    assert.equal(resolveSwarmPrompt(dir, { task_id: 'id-1', from: 'a', transfer_send_to: '', attempt: 1 }, 'do X'), null);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test('swarm prompt: template file overrides with placeholders', async () => {
  const { resolveSwarmPrompt } = await import('../dist/swarm-prompt.js');
  const dir = mkdtempSync(join(tmpdir(), 'pi-onlyne-prompt-'));
  try {
    mkdirSync(join(dir, '.pi'), { recursive: true });
    mkdirSync(join(dir, 'prompts'), { recursive: true });
    writeFileSync(join(dir, '.pi', 'onlyne.json'), '{"swarm_prompt": {"template": "prompts/swarm-task.md"}}');
    writeFileSync(join(dir, 'prompts', 'swarm-task.md'), 'TASK {task_id} FROM {from}:\n{payload}\nDo it, then swarm_complete.');
    const out = resolveSwarmPrompt(dir, { task_id: 'id-9', from: 'b', transfer_send_to: '', attempt: 2 }, 'relay');
    assert.ok(out.includes('TASK id-9 FROM b'));
    assert.ok(out.includes('relay'));
    assert.ok(!out.includes('Restore context'));
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test('swarm ctl: recycle wire parses, task parser rejects it', async () => {
  const { parseSwarmCtl } = await import('../dist/swarm.js');
  const { parseSwarmHeader } = await import('../dist/swarm.js');
  const wire = '---swarm-ctl\nop: recycle\ntask_id: abc\nreason: cancel\n---\n';
  const ctl = parseSwarmCtl(wire);
  assert.ok(ctl);
  assert.equal(ctl.op, 'recycle');
  assert.equal(ctl.task_id, 'abc');
  assert.equal(ctl.reason, 'cancel');
  assert.equal(parseSwarmHeader(wire), null);
  assert.equal(parseSwarmCtl('---swarm\ntask_id: x\n---\n'), null);
  assert.equal(parseSwarmCtl('---swarm-ctl\nop: nope\ntask_id: x\n---\n'), null);
});
