import { mkdir, readFile, rename, writeFile } from 'node:fs/promises';
import { randomUUID } from 'node:crypto';
import path from 'node:path';
import type { Bot, Conversation, DesktopSettings, DesktopState, DesktopStore } from './contracts.ts';
import { shortId } from './ids.ts';

const STATE_VERSION = 1;

function assertVersion1State(value: unknown, file: string): DesktopState {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    throw new Error(`desktop store: ${file} is not a JSON object`);
  }
  const state = value as DesktopState;
  if (state.version !== STATE_VERSION) {
    throw new Error(`desktop store: ${file} has unsupported version ${JSON.stringify(state.version)}, expected ${STATE_VERSION}`);
  }
  for (const field of ['bots', 'conversations', 'messages', 'capabilities'] as const) {
    if (!Array.isArray(state[field])) {
      throw new Error(`desktop store: ${file} is missing a valid "${field}" array`);
    }
  }
  if (typeof state.settings !== 'object' || state.settings === null || Array.isArray(state.settings)) {
    throw new Error(`desktop store: ${file} is missing a valid "settings" object`);
  }
  if (!Array.isArray(state.media)) state.media = [];
  if (typeof state.teamAdminId !== 'string' || !state.bots.some(bot => bot.id === state.teamAdminId)) {
    state.teamAdminId = state.bots[0]?.id;
  }
  return state;
}

function defaultState(dataDir: string): DesktopState {
  const now = Date.now();
  const botId = shortId('bot');
  const workspace = path.join(dataDir, 'workspaces', botId);
  const assistant: Bot = {
    id: botId,
    name: 'Assistant',
    description: 'Default desktop bot',
    instructions: '',
    memory: '',
    workspace,
    thinking: 'inherit',
    capabilities: [],
    runtime: 'omp-rpc',
    runtimeOptions: {},
    createdAt: now,
    updatedAt: now,
  };
  const conversation: Conversation = {
    id: shortId('c'),
    title: 'Assistant',
    kind: 'direct',
    botIds: [assistant.id],
    adminBotId: assistant.id,
    memory: '',
    createdAt: now,
    updatedAt: now,
  };
  const settings: DesktopSettings = {
    theme: 'dark',
    locale: 'zh-CN',
    hostPlugins: [],
    rendererPlugins: [],
    customCss: '',
    nodeId: `urn:onlyne:agent:desktop:${shortId()}`,
    peers: [],
  };
  return {
    version: STATE_VERSION,
    bots: [assistant],
    teamAdminId: assistant.id,
    conversations: [conversation],
    messages: [],
    capabilities: [],
    media: [],
    settings,
  };
}

export async function openDesktopStore(dataDir: string): Promise<DesktopStore> {
  await mkdir(dataDir, { recursive: true });
  await mkdir(path.join(dataDir, 'workspaces'), { recursive: true });
  const file = path.join(dataDir, 'state.json');
  let committed: DesktopState;
  let text: string | null = null;
  try {
    text = await readFile(file, 'utf8');
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'ENOENT') {
      throw new Error(`desktop store: cannot read ${file}: ${(error as Error).message}`);
    }
  }
  if (text === null) {
    const fresh = defaultState(dataDir);
    await mkdir(path.join(dataDir, 'workspaces', fresh.bots[0]!.id), { recursive: true });
    await writeAtomic(file, fresh);
    committed = fresh;
  } else {
    let parsed: unknown;
    try {
      parsed = JSON.parse(text);
    } catch (error) {
      throw new Error(`desktop store: ${file} is not valid JSON: ${(error as Error).message}`);
    }
    committed = assertVersion1State(parsed, file);
  }

  let chain: Promise<unknown> = Promise.resolve();
  let closed = false;

  async function writeAtomic(target: string, state: DesktopState): Promise<void> {
    const tmp = `${target}.${randomUUID()}.tmp`;
    await writeFile(tmp, JSON.stringify(state, null, 2) + '\n', 'utf8');
    await rename(tmp, target);
  }

  const store: DesktopStore = {
    read(): DesktopState {
      return structuredClone(committed);
    },
    update<T>(change: (state: DesktopState) => T): Promise<T> {
      if (closed) {
        return Promise.reject(new Error('desktop store: store is closed'));
      }
      const run = chain.then(async (): Promise<T> => {
        const draft = structuredClone(committed);
        const result = await change(draft);
        await writeAtomic(file, draft);
        committed = draft;
        return result;
      });
      chain = run.catch(() => undefined);
      return run;
    },
    close(): Promise<void> {
      closed = true;
      return chain.then(() => undefined);
    },
  };
  return store;
}
