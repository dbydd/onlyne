import { lstat, mkdir, readdir, readFile, realpath, rename, writeFile } from 'node:fs/promises';
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { botConfigDir } from './bot-config.ts';
import { homedir } from 'node:os';

export async function configurationCommand(dataDir: string, botId: string, action: string, input: Record<string, unknown>): Promise<unknown> {
  const root = botConfigDir(dataDir, botId);
  if (action === 'config.hindsightDefaults') {
    const candidates = ['omp-hindsight.json', 'omp-hindsight.jsonc', 'hindsight-omp.json', 'hindsight-omp.jsonc'];
    let inherited: Record<string, unknown> = {};
    let globalProblem: string | undefined;
    for (const name of candidates) {
      let source: string;
      try { source = await readFile(join(homedir(), '.omp', 'agent', name), 'utf8'); }
      catch (error) {
        if (error instanceof Error && 'code' in error && error.code === 'ENOENT') continue;
        throw error;
      }
      try {
        const value: unknown = Bun.JSONC.parse(source);
        if (value === null || typeof value !== 'object' || Array.isArray(value)) throw new Error(`Invalid inherited Hindsight configuration: ${name}`);
        inherited = value as Record<string, unknown>;
      } catch (error) { globalProblem = error instanceof Error ? error.message : String(error); }
      break;
    }
    const globalBankIds: string[] = [];
    if (Array.isArray(inherited.banks)) for (const bank of inherited.banks) {
      if (bank !== null && typeof bank === 'object' && 'id' in bank && typeof bank.id === 'string' && !globalBankIds.includes(bank.id)) globalBankIds.push(bank.id);
    }
    for (const name of candidates) {
      try {
        const content = await readFile(join(root, name), 'utf8');
        Bun.JSONC.parse(content);
        return { path: name, content, exists: true, globalBankIds, ...(globalProblem === undefined ? {} : { globalProblem }) };
      } catch (error) {
        if (error instanceof Error && 'code' in error && error.code === 'ENOENT') continue;
        throw error;
      }
    }
    return { path: 'omp-hindsight.json', exists: false, globalBankIds, ...(globalProblem === undefined ? {} : { globalProblem }) };
  }
  const value = input.path === undefined ? '' : input.path;
  if (typeof value !== 'string' || isAbsolute(value)) throw new Error('Configuration path must be relative to the bot .omp directory');
  const path = resolve(root, value);
  const name = relative(root, path);
  if (name === '..' || name.startsWith(`..${sep}`)) throw new Error('Configuration path leaves this bot');
  const parts = name.split(sep);
  if (parts[0] === 'onlyne' || parts.some(part => part.endsWith('.db') || part.endsWith('.db-wal') || part.endsWith('.db-shm'))) {
    throw new Error('Use the Onlyne panel for node state and pairing');
  }
  const rootPath = await realpath(root);
  let ancestor = path;
  for (;;) {
    try {
      const canonical = await realpath(ancestor);
      const offset = relative(rootPath, canonical);
      if (offset === '..' || offset.startsWith(`..${sep}`) || isAbsolute(offset)) throw new Error('Configuration path follows a link outside this bot');
      break;
    } catch (error) {
      if (!(error instanceof Error) || !('code' in error) || error.code !== 'ENOENT') throw error;
      const parent = dirname(ancestor);
      if (parent === ancestor) throw error;
      ancestor = parent;
    }
  }
  if (action === 'config.list') {
    const entries = await readdir(path, { withFileTypes: true });
    const visible = entries.filter(entry => entry.name !== 'onlyne' && !/\.db(?:-wal|-shm)?$/.test(entry.name));
    return { entries: visible.map(entry => ({ name: entry.name, path: join(name, entry.name), kind: entry.isDirectory() ? 'directory' : 'file' })) };
  }
  if (name.length === 0) throw new Error('Choose a configuration file');
  if (action === 'config.read') {
    const info = await lstat(path);
    if (!info.isFile()) throw new Error('Choose a regular configuration file');
    return { path: name, content: await readFile(path, 'utf8') };
  }
  if (action === 'config.write') {
    if (typeof input.content !== 'string') throw new Error('Configuration content must be text');
    if (name.endsWith('.json')) JSON.parse(input.content);
    await mkdir(dirname(path), { recursive: true });
    const temporary = `${path}.${crypto.randomUUID()}.tmp`;
    await writeFile(temporary, input.content, { mode: 0o600 });
    await rename(temporary, path);
    return { saved: true, restartRequired: true };
  }
  throw new Error(`Unknown configuration action: ${action}`);
}
