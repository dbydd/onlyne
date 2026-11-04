#!/usr/bin/env bun
import type { Subprocess } from 'bun';
import { createRequire } from 'node:module';
import {
  closeSync, existsSync, mkdirSync, openSync, readFileSync, renameSync,
  unlinkSync, writeFileSync, writeSync,
} from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const PACKAGE_ROOT = dirname(HERE);
const BACKEND_ENTRY = join(HERE, 'main.ts');
const SHELL_ENTRY = join(HERE, 'electron', 'main.cjs');
const LOCK_FILE = 'shell.lock.json';
const READY_TIMEOUT_MS = 30_000;
const STOP_TIMEOUT_MS = 5_000;
const LOOPBACK_HOSTS: Record<string, true> = {
  '127.0.0.1': true,
  localhost: true,
  '[::1]': true,
  '::1': true,
};
const SIGNALS = ['SIGINT', 'SIGTERM'] as const;

type BackendChild = Subprocess<'ignore', 'pipe' | 'inherit', 'inherit'>;
type ShellChild = Subprocess<'ignore', 'pipe', 'inherit'>;
type Child = BackendChild | ShellChild;

interface LaunchOptions {
  serve: boolean;
  dataDir: string;
  port?: number;
}
interface ShellLock {
  pid: number;
  url: string | null;
  shell: number | null;
}
interface RunState {
  backend: BackendChild | null;
  shell: ShellChild | null;
  stopping: boolean;
}

const USAGE = `Onlyne Desktop

  bun src/launch.ts [--data-dir <dir>] [--port <port>]
  bun src/launch.ts --serve [--data-dir <dir>] [--port <port>]

  --data-dir  bots, conversations and OMP sessions (default: ${defaultDataDir()})
  --port      backend port; the operating system picks one when omitted
  --serve     run the loopback backend only, without the desktop window
  --help      show this help
`;

function defaultDataDir(): string {
  const home = homedir();
  if (process.platform === 'darwin') {
    return join(home, 'Library', 'Application Support', 'Onlyne Desktop', 'node');
  }
  if (process.platform === 'win32') {
    return join(process.env.LOCALAPPDATA ?? join(home, 'AppData', 'Local'), 'Onlyne Desktop', 'node');
  }
  const dataHome = process.env.XDG_DATA_HOME;
  return join(dataHome && dataHome.length > 0 ? dataHome : join(home, '.local', 'share'), 'onlyne', 'desktop');
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

async function writeAndFlush(stream: NodeJS.WritableStream, text: string): Promise<void> {
  const done = Promise.withResolvers<void>();
  stream.write(text, () => done.resolve());
  await done.promise;
}

function loopbackUrl(raw: string): URL | null {
  let url: URL;
  try { url = new URL(raw); } catch { return null; }
  if (url.protocol !== 'http:') return null;
  if (LOOPBACK_HOSTS[url.hostname] !== true) return null;
  if (url.port === '' || url.username !== '' || url.password !== '') return null;
  return url;
}

function parseArgs(argv: string[]): LaunchOptions | null {
  let serve = false;
  let dataDir: string | undefined;
  let port: number | undefined;
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === undefined) continue;
    if (arg === '--help' || arg === '-h') return null;
    if (arg === '--serve') { serve = true; continue; }
    if (arg === '--data-dir' || arg.startsWith('--data-dir=')) {
      const value = arg === '--data-dir' ? argv[++index] : arg.slice('--data-dir='.length);
      if (value === undefined || value === '') throw new Error('--data-dir needs a directory.');
      dataDir = value;
      continue;
    }
    if (arg === '--port' || arg.startsWith('--port=')) {
      const value = arg === '--port' ? argv[++index] : arg.slice('--port='.length);
      if (value === undefined || !/^\d+$/.test(value) || Number(value) > 65_535) {
        throw new Error('--port needs an integer between 0 and 65535.');
      }
      port = Number(value);
      continue;
    }
    throw new Error(`Unknown argument: ${arg}`);
  }
  return {
    serve,
    dataDir: resolve(dataDir ?? defaultDataDir()),
    ...(port === undefined ? {} : { port }),
  };
}

function resolveElectronPath(): string {
  let binary: unknown;
  try {
    binary = createRequire(import.meta.url)('electron');
  } catch (error) {
    throw new Error(`The electron package is not installed: ${errorMessage(error)}. Install this package's dependencies in ${PACKAGE_ROOT}.`);
  }
  if (typeof binary !== 'string' || binary === '') {
    throw new Error('The electron package did not report an executable path.');
  }
  if (!existsSync(binary)) {
    throw new Error(`Electron's binary is missing at ${binary}; its postinstall download did not run. Allow electron in trustedDependencies and install again.`);
  }
  return binary;
}

function spawnBackend(options: LaunchOptions, stdout: 'pipe' | 'inherit' = 'pipe'): BackendChild {
  const args = [
    BACKEND_ENTRY,
    '--serve',
    '--data-dir', options.dataDir,
    ...(options.port === undefined ? [] : ['--port', String(options.port)]),
  ];
  const env = { ...process.env };
  delete env.ONLYNE_DESKTOP_URL;
  return Bun.spawn({
    cmd: [process.execPath, ...args],
    cwd: PACKAGE_ROOT,
    env,
    stdin: 'ignore',
    stdout,
    stderr: 'inherit',
  });
}

function spawnShell(electronPath: string, url: string): ShellChild {
  const env: NodeJS.ProcessEnv = { ...process.env, ONLYNE_DESKTOP_URL: url };
  // Parent IDEs set this to run Electron as Node; it would leave the app without a window.
  delete env.ELECTRON_RUN_AS_NODE;
  return Bun.spawn({
    cmd: [electronPath, SHELL_ENTRY],
    cwd: PACKAGE_ROOT,
    env,
    stdio: ['ignore', 'pipe', 'inherit'],
  });
}

interface ReadyMessage {
  type: 'desktop.ready';
  url: string;
  port: number;
}
interface ShellReadyMessage {
  type: 'desktop.shell.ready';
}

function isReadyMessage(value: unknown): value is ReadyMessage {
  if (typeof value !== 'object' || value === null || !('type' in value)) return false;
  if (value.type !== 'desktop.ready') return false;
  if (!('url' in value) || typeof value.url !== 'string') return false;
  return 'port' in value && typeof value.port === 'number' && Number.isInteger(value.port);
}

function isShellReadyMessage(value: unknown): value is ShellReadyMessage {
  if (typeof value !== 'object' || value === null || !('type' in value)) return false;
  return value.type === 'desktop.shell.ready';
}

/**
 * Waits for one JSON line the child announces itself with. Every other line is forwarded
 * to stderr, and the rest of the stream keeps draining so a chatty child never blocks.
 */
async function waitForJsonLine<T>(
  child: Child,
  accept: (value: unknown) => value is T,
  timeoutMs: number,
  label: string,
): Promise<T> {
  const reader = (child.stdout as unknown as ReadableStream<Uint8Array>).getReader();
  const decoder = new TextDecoder();
  let buffer = '';

  const readLine = async (): Promise<string | null> => {
    for (;;) {
      const index = buffer.indexOf('\n');
      if (index >= 0) {
        const line = buffer.slice(0, index).replace(/\r$/, '');
        buffer = buffer.slice(index + 1);
        return line;
      }
      const chunk = await reader.read();
      if (chunk.done === true) {
        if (buffer === '') return null;
        const rest = buffer;
        buffer = '';
        return rest;
      }
      buffer += decoder.decode(chunk.value, { stream: true });
    }
  };
  const drain = async (): Promise<void> => {
    for (;;) {
      const line = await readLine();
      if (line === null) return;
      const text = line.trim();
      if (text !== '') process.stderr.write(`${text}\n`);
    }
  };

  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const remaining = deadline - Date.now();
    if (remaining <= 0) throw new Error(`The ${label} did not announce itself within ${timeoutMs / 1000}s.`);
    const outcome = await Promise.race([
      readLine().then(line => ({ kind: 'line' as const, line })),
      child.exited.then(code => ({ kind: 'exit' as const, code })),
      Bun.sleep(remaining).then(() => ({ kind: 'timeout' as const })),
    ]);
    if (outcome.kind === 'exit') throw new Error(`The ${label} exited with code ${outcome.code} before it announced itself.`);
    if (outcome.kind === 'timeout') throw new Error(`The ${label} did not announce itself within ${timeoutMs / 1000}s.`);
    if (outcome.line === null) throw new Error(`The ${label} closed stdout before it announced itself.`);
    const text = outcome.line.trim();
    if (text === '') continue;
    let parsed: unknown = null;
    try { parsed = JSON.parse(text); } catch { parsed = null; }
    if (parsed !== null && accept(parsed)) {
      void drain().catch(() => { /* the child is closing */ });
      return parsed;
    }
    process.stderr.write(`${text}\n`);
  }
}

async function waitForBackend(backend: BackendChild): Promise<URL> {
  const message = await waitForJsonLine(backend, isReadyMessage, READY_TIMEOUT_MS, 'backend');
  const url = loopbackUrl(message.url);
  if (url === null) throw new Error('The backend announced a URL that is not a loopback HTTP URL.');
  return url;
}

function createLock(lockPath: string): boolean {
  try {
    const fd = openSync(lockPath, 'wx', 0o600);
    try { writeSync(fd, JSON.stringify({ pid: process.pid, url: null, shell: null })); }
    finally { closeSync(fd); }
    return true;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'EEXIST') return false;
    throw error;
  }
}

function readLock(lockPath: string): ShellLock | null {
  let text: string;
  try { text = readFileSync(lockPath, 'utf8'); } catch { return null; }
  let value: unknown;
  try { value = JSON.parse(text); } catch { return null; }
  if (typeof value !== 'object' || value === null || !('pid' in value)) return null;
  if (typeof value.pid !== 'number' || !Number.isInteger(value.pid) || value.pid <= 1) return null;
  const url = 'url' in value && typeof value.url === 'string' ? value.url : null;
  const shell = 'shell' in value && typeof value.shell === 'number' && Number.isInteger(value.shell) && value.shell > 1
    ? value.shell
    : null;
  return { pid: value.pid, url, shell };
}

function writeLock(lockPath: string, value: ShellLock): void {
  // Rename keeps a concurrent launcher from reading a half-written lock.
  const temp = `${lockPath}.${process.pid}.tmp`;
  writeFileSync(temp, JSON.stringify(value), { mode: 0o600 });
  renameSync(temp, lockPath);
}

function releaseLock(lockPath: string): void {
  const current = readLock(lockPath);
  if (current === null || current.pid !== process.pid) return;
  try { unlinkSync(lockPath); } catch { /* another launcher already replaced it */ }
}

function isAlive(pid: number): boolean {
  try { process.kill(pid, 0); return true; }
  catch (error) { return (error as NodeJS.ErrnoException).code === 'EPERM'; }
}

type Claim = { kind: 'owner' } | { kind: 'focus'; url: string };

async function claimShellSlot(lockPath: string, timeoutMs: number): Promise<Claim> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    if (createLock(lockPath)) return { kind: 'owner' };
    const current = readLock(lockPath);
    if (current !== null && !isAlive(current.pid)) {
      try { unlinkSync(lockPath); }
      catch (error) {
        if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
      }
      continue;
    }
    // An owner whose window is gone is already shutting its backend down; wait for its lock.
    if (current !== null && current.shell !== null && !isAlive(current.shell)) {
      if (Date.now() >= deadline) {
        throw new Error(`Another Onlyne Desktop instance holds ${lockPath} without a window; it did not release it within ${timeoutMs / 1000}s.`);
      }
      await Bun.sleep(100);
      continue;
    }
    if (current !== null && current.shell !== null && current.url !== null) {
      const url = loopbackUrl(current.url);
      if (url === null) {
        throw new Error(`Another Onlyne Desktop instance holds ${lockPath} with a URL that is not loopback; refusing to reuse it.`);
      }
      return { kind: 'focus', url: url.href };
    }
    if (Date.now() >= deadline) {
      throw new Error(`Another Onlyne Desktop instance holds ${lockPath} but did not report a URL within ${timeoutMs / 1000}s.`);
    }
    await Bun.sleep(100);
  }
}

async function stopChild(child: Child): Promise<void> {
  if (child.exitCode !== null) return;
  try { child.kill('SIGTERM'); } catch { return; }
  const exited = await Promise.race([
    child.exited.then(() => true),
    Bun.sleep(STOP_TIMEOUT_MS).then(() => false),
  ]);
  if (!exited) {
    try { child.kill('SIGKILL'); } catch { return; }
    await child.exited.catch(() => 0);
  }
}

async function stopShell(state: RunState): Promise<void> {
  const shell = state.shell;
  state.shell = null;
  if (shell !== null) await stopChild(shell);
}

async function stopBackend(state: RunState): Promise<void> {
  const backend = state.backend;
  state.backend = null;
  if (backend !== null) await stopChild(backend);
}

async function stopAll(state: RunState): Promise<void> {
  await stopShell(state);
  await stopBackend(state);
}

async function runBackend(options: LaunchOptions): Promise<number> {
  const child = spawnBackend(options, 'inherit');
  const forward = (signal: typeof SIGNALS[number]): void => {
    try { child.kill(signal); } catch { /* the child already exited */ }
  };
  const onInt = (): void => forward('SIGINT');
  const onTerm = (): void => forward('SIGTERM');
  process.on('SIGINT', onInt);
  process.on('SIGTERM', onTerm);
  try { return await child.exited; }
  finally {
    process.off('SIGINT', onInt);
    process.off('SIGTERM', onTerm);
  }
}

async function runOwnedShell(
  electronPath: string,
  lockPath: string,
  options: LaunchOptions,
): Promise<number> {
  const state: RunState = { backend: null, shell: null, stopping: false };
  const onSignal = (): void => {
    if (state.stopping) return;
    state.stopping = true;
    void stopAll(state).then(() => {
      releaseLock(lockPath);
      process.exit(0);
    });
  };
  for (const signal of SIGNALS) process.on(signal, onSignal);
  try {
    const backend = spawnBackend(options);
    state.backend = backend;
    const launchUrl = await waitForBackend(backend);
    // The launch URL holds a one-use secret. Only the origin is durable and reusable.
    const origin = new URL('/', launchUrl);
    process.stderr.write(`Onlyne Desktop: backend on 127.0.0.1:${launchUrl.port}\n`);

    const shell = spawnShell(electronPath, launchUrl.href);
    state.shell = shell;
    // Written only after the window exists: a second launch must never race this Electron
    // for the single-instance lock, or it would become the owner of a spent session.
    await waitForJsonLine(shell, isShellReadyMessage, READY_TIMEOUT_MS, 'desktop window');
    writeLock(lockPath, { pid: process.pid, url: origin.href, shell: shell.pid });
    const outcome = await Promise.race([
      shell.exited.then(code => ({ source: 'shell' as const, code })),
      backend.exited.then(code => ({ source: 'backend' as const, code })),
    ]);
    if (outcome.source === 'shell') {
      await stopBackend(state);
      return outcome.code;
    }
    process.stderr.write(`Onlyne Desktop: the backend exited (${outcome.code}); closing the window.\n`);
    await stopAll(state);
    return outcome.code === 0 ? 1 : outcome.code;
  } catch (error) {
    await stopAll(state);
    if (state.stopping) return 0;
    throw error;
  } finally {
    for (const signal of SIGNALS) process.off(signal, onSignal);
    releaseLock(lockPath);
  }
}

async function runDesktop(options: LaunchOptions): Promise<number> {
  mkdirSync(options.dataDir, { recursive: true });
  const lockPath = join(options.dataDir, LOCK_FILE);
  const electronPath = resolveElectronPath();
  const claim = await claimShellSlot(lockPath, READY_TIMEOUT_MS);
  if (claim.kind === 'focus') {
    const shell = spawnShell(electronPath, claim.url);
    // This instance only hands focus to the running app and quits; drain its stdout so a
    // chatty handoff never blocks it.
    void waitForJsonLine(shell, isShellReadyMessage, READY_TIMEOUT_MS, 'desktop window')
      .catch(() => { /* exiting without a window is the expected handoff */ });
    return await shell.exited;
  }
  return await runOwnedShell(electronPath, lockPath, options);
}

async function main(): Promise<number> {
  let options: LaunchOptions | null = null;
  try {
    options = parseArgs(process.argv.slice(2));
  } catch (error) {
    await writeAndFlush(process.stderr, `Onlyne Desktop: ${errorMessage(error)}\n\n${USAGE}`);
    return 2;
  }
  if (options === null) {
    await writeAndFlush(process.stdout, USAGE);
    return 0;
  }
  if (options.serve) return await runBackend(options);
  try {
    return await runDesktop(options);
  } catch (error) {
    await writeAndFlush(process.stderr, `Onlyne Desktop: ${errorMessage(error)}\n`);
    return 1;
  }
}

process.exit(await main());
