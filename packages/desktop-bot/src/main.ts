#!/usr/bin/env bun
import { homedir } from 'node:os';
import { join, resolve } from 'node:path';
import { mkdir, open, readFile, unlink } from 'node:fs/promises';
import { parseArgs } from 'node:util';
import { createDesktopApp } from './app.ts';
import { createDemoOnlyne, ensureDemoTeam } from './demo.ts';
import { startDesktopServer } from './server.ts';
import { openDesktopStore } from './store.ts';
import type { DesktopController, DesktopServer } from './contracts.ts';

export function defaultDataDir(): string {
  if (process.platform === 'darwin') return join(homedir(), 'Library', 'Application Support', 'Onlyne Desktop', 'node');
  if (process.platform === 'win32') return join(process.env.LOCALAPPDATA ?? join(homedir(), 'AppData', 'Local'), 'Onlyne Desktop', 'node');
  return join(process.env.XDG_DATA_HOME ?? join(homedir(), '.local', 'share'), 'onlyne', 'desktop');
}

/** Separate from a live desk so demo records never overwrite a real team. */
export function demoDataDir(): string {
  return join(defaultDataDir(), '..', 'desktop-demo');
}

if (import.meta.main) {
  const { values } = parseArgs({ args: process.argv.slice(2), options: {
    serve: { type: 'boolean' }, demo: { type: 'boolean' }, 'data-dir': { type: 'string' }, port: { type: 'string' }, help: { type: 'boolean' },
  } });
  if (values.help) {
    console.log('Usage: onlyne-desktop [--serve] [--demo] [--data-dir <directory>] [--port <port>]');
    process.exit(0);
  }
  const dataDir = resolve(values['data-dir'] ?? (values.demo ? demoDataDir() : defaultDataDir()));
  const port = values.port === undefined ? 0 : Number(values.port);
  if (!Number.isInteger(port) || port < 0 || port > 65535) throw new Error('port must be an integer from 0 to 65535');
  await mkdir(dataDir, { recursive: true });
  const lockPath = join(dataDir, 'backend.lock');
  const acquire = () => open(lockPath, 'wx', 0o600);
  let lock;
  try { lock = await acquire(); }
  catch (error) {
    if (!(error instanceof Error) || !('code' in error) || error.code !== 'EEXIST') throw error;
    const pid = Number((await readFile(lockPath, 'utf8')).trim());
    if (!Number.isSafeInteger(pid) || pid <= 0) throw new Error(`Invalid desktop lock: ${lockPath}`);
    let alive = true;
    try { process.kill(pid, 0); }
    catch (probe) {
      if (probe instanceof Error && 'code' in probe && probe.code === 'ESRCH') alive = false;
      else throw probe;
    }
    if (alive) throw new Error(`Onlyne Desktop already uses this data directory (PID ${pid})`);
    await unlink(lockPath);
    lock = await acquire();
  }
  await lock.writeFile(String(process.pid));
  let app: DesktopController | undefined;
  let server: DesktopServer | undefined;
  let shutdown: Promise<void> | undefined;
  const close = (): Promise<void> => shutdown ??= (async () => {
    await server?.close();
    await app?.close();
    await lock.close();
    await unlink(lockPath);
  })();
  process.once('SIGINT', () => { void close().then(() => process.exit(0), error => { console.error(error); process.exit(1); }); });
  process.once('SIGTERM', () => { void close().then(() => process.exit(0), error => { console.error(error); process.exit(1); }); });
  try {
    if (values.demo) {
      const seeding = await openDesktopStore(dataDir);
      try { await ensureDemoTeam(seeding, dataDir); }
      finally { await seeding.close(); }
    }
    const controller = await createDesktopApp({
      dataDir,
      ...(values.demo ? { onlyne: createDemoOnlyne, demo: true } : {}),
    });
    app = controller;
    const botRenderers = new Map(controller.snapshot().bots.map(bot => [
      bot.id,
      (bot.plugins?.renderer ?? []).map(source =>
        /^(https?:)?\/\//.test(source) ? '' : resolve(bot.configDir ?? resolve(dataDir, 'bots', bot.id, '.omp'), source)),
    ]));
    const media = async (id: string) => {
      const item = controller.snapshot().media.find(entry => entry.id === id);
      if (item === undefined || item.url !== undefined) return undefined;
      return { path: resolve(dataDir, 'media', item.id), contentType: item.contentType };
    };
    server = await startDesktopServer({ controller, port, botRenderers, media });
    console.log(JSON.stringify({ type: 'desktop.ready', url: server.url, port: server.port, demo: values.demo === true }));
    console.error(`Open ${server.url}`);
  } catch (error) {
    await close();
    throw error;
  }
}
