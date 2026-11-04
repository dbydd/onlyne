import { mkdir, readFile, rename, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import type { Bot } from './contracts.ts';
import { botNodeId } from './ids.ts';

export function botConfigDir(dataDir: string, botId: string): string {
  if (!/^[a-zA-Z0-9_-]+$/.test(botId)) throw new Error('Invalid bot configuration identity');
  return join(dataDir, 'bots', botId, '.omp');
}

async function atomicText(path: string, content: string): Promise<void> {
  let current: string | undefined;
  try { current = await readFile(path, 'utf8'); }
  catch (error) {
    if (!(error instanceof Error) || !('code' in error) || error.code !== 'ENOENT') throw error;
  }
  if (current === content) return;
  const temporary = `${path}.${crypto.randomUUID()}.tmp`;
  await writeFile(temporary, content, { mode: 0o600 });
  await rename(temporary, path);
}

export async function saveBotConfiguration(dataDir: string, bot: Bot): Promise<Bot> {
  const configDir = botConfigDir(dataDir, bot.id);
  await mkdir(join(configDir, 'memory'), { recursive: true });
  const config: Bot = {
    ...bot, configDir,
    onlyne: bot.onlyne ?? { nodeId: botNodeId(bot.id), peers: [] },
    plugins: bot.plugins ?? { host: [], renderer: [] },
  };
  const { memory, configDir: _configDir, ...document } = config;
  await atomicText(join(configDir, 'memory', 'MEMORY.md'), memory);
  await atomicText(join(configDir, 'bot.json'), `${JSON.stringify(document, null, 2)}\n`);
  return config;
}

export async function loadBotConfiguration(dataDir: string, bot: Bot): Promise<Bot> {
  const configDir = botConfigDir(dataDir, bot.id);
  let document: unknown;
  try { document = JSON.parse(await readFile(join(configDir, 'bot.json'), 'utf8')); }
  catch (error) {
    if (error instanceof Error && 'code' in error && error.code === 'ENOENT') return saveBotConfiguration(dataDir, bot);
    throw new Error(`Cannot read bot configuration ${join(configDir, 'bot.json')}: ${error instanceof Error ? error.message : String(error)}`);
  }
  if (document === null || typeof document !== 'object' || Array.isArray(document)) throw new Error(`Invalid bot configuration: ${configDir}`);
  if (!('id' in document) || document.id !== bot.id) throw new Error(`Bot configuration identity differs from its directory: ${configDir}`);
  for (const field of ['name', 'description', 'instructions', 'workspace', 'runtime', 'thinking']) {
    if (!(field in document) || typeof Reflect.get(document, field) !== 'string') throw new Error(`Bot configuration needs text field ${field}: ${configDir}`);
  }
  if (!('capabilities' in document) || !Array.isArray(document.capabilities) || !document.capabilities.every(value => typeof value === 'string')) throw new Error(`Invalid bot capabilities: ${configDir}`);
  const configured = document as Omit<Bot, 'memory'>;
  const memory = await readFile(join(configDir, 'memory', 'MEMORY.md'), 'utf8');
  return { ...bot, ...configured, memory, configDir };
}
