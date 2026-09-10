import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { loadConfig, saveConfig, inboundModeFor } from '../dist/config.js';
import { connectDaemon } from '../dist/onlyne.js';
import { findWorkspace } from '../dist/workspace.js';

test('config defaults and rules', () => {
  const dir = mkdtempSync(join(tmpdir(), 'pi-onlyne-'));
  try {
    const cfg = loadConfig(dir);
    assert.equal(cfg.watch.autoStart, false);
    cfg.inbound.rules.push({ channel: 'tg', mode: 'muted' });
    cfg.inbound.rules.push({ channel: 'tg', conversation: '1', mode: 'queue-only' });
    saveConfig(dir, cfg);
    const loaded = loadConfig(dir);
    assert.equal(inboundModeFor(loaded, 'tg', '1'), 'queue-only');
    assert.equal(inboundModeFor(loaded, 'tg', '2'), 'muted');
    assert.equal(inboundModeFor(loaded, 'wx', 'x'), 'auto-handle');
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test('connectDaemon can require an already running workspace daemon', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'pi-onlyne-'));
  try {
    await assert.rejects(
      connectDaemon({ root: dir, onlyneDir: join(dir, '.onlyne'), socketPath: join(dir, '.onlyne/run/onlyne.sock') }, false),
      /onlyne daemon is not running/
    );
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test('workspace discovery finds parent .onlyne only', () => {
  const dir = mkdtempSync(join(tmpdir(), 'pi-onlyne-'));
  try {
    assert.equal(findWorkspace(dir), null);
    mkdirSync(join(dir, '.onlyne'));
    mkdirSync(join(dir, 'a/b'), { recursive: true });
    assert.equal(findWorkspace(join(dir, 'a/b')).root, dir);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});
