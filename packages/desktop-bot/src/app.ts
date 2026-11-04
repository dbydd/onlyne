import { mkdir, readdir, readFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { createHash } from 'node:crypto';
import {
  DEFAULT_MAX_AUTO_TURNS,
  type Bot, type Capability, type ChatMessage, type Conversation, type DesktopController, type DesktopEvent, type DesktopPlugin,
  type DesktopPluginApi, type DesktopState, type DesktopStore, type HostTool, type HostToolContext, type HostUri, type Json,
  type MediaItem, type OnlyneBridge, type OnlyneBridgeOptions, type RpcFrame, type RuntimeConnection, type RuntimeDriver, type WidgetEnvelope,
} from './contracts.ts';
import { openDesktopStore } from './store.ts';
import { ompRpcDriver } from './rpc.ts';
import { createDesktopOnlyne } from './onlyne.ts';
import { createTeamTools } from './team-tools.ts';
import { GroupTurnScheduler } from './turn-scheduler.ts';
import { botConfigDir, loadBotConfiguration, saveBotConfiguration } from './bot-config.ts';
import { configurationCommand } from './config-files.ts';
import { characterToJson, normalizeCharacter, parseCharacterJson, parseLorebook, parsePngCharacter } from './character.ts';
import { compileScene } from './harness.ts';
import { createMediaService, mediaKind, resolveMediaPath, stickerPath } from './media.ts';
import { shortId } from './ids.ts';

const record = (value: unknown): Record<string, unknown> => {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) throw new Error('Expected an object');
  return value as Record<string, unknown>;
};
const text = (value: unknown, field: string): string => {
  if (typeof value !== 'string' || value.trim().length === 0) throw new Error(`${field} must be nonempty text`);
  return value;
};
const identifier = (value: unknown): string => {
  const result = text(value, 'id');
  if (!/^[a-zA-Z0-9_-]+$/.test(result)) throw new Error('id must contain letters, digits, underscores or hyphens');
  return result;
};
const errorText = (error: unknown): string => error instanceof Error ? error.message : String(error);
function demoLine(bot: { id: string; name: string }, spoken: string): string {
  const line = spoken.length > 0 ? spoken.slice(0, 80) : 'that';
  if (bot.id === 'mira') return `“${line}” stays on Trust. I will not type a fingerprint for you.`;
  if (bot.id === 'jun') return `“${line}” needs a source. I will not add a tool from this chat.`;
  if (bot.id === 'lark') return `“${line}” is not a credential. I only report the gateway card.`;
  return `${bot.name}: “${line}”. This is a short demo reply.`;
}
const contentText = (value: unknown): string => {
  if (!Array.isArray(value)) return '';
  const parts: string[] = [];
  for (const part of value) {
    if (part !== null && typeof part === 'object' && 'type' in part && part.type === 'text'
      && 'text' in part && typeof part.text === 'string') parts.push(part.text);
  }
  return parts.join('\n');
};
const toolResult = (value: unknown) => ({ content: [{ type: 'text', text: JSON.stringify(value, null, 2) }] });

export interface DesktopAppOptions {
  dataDir: string;
  store?: DesktopStore;
  drivers?: RuntimeDriver[];
  plugins?: DesktopPlugin[];
  onlyne?: (options: OnlyneBridgeOptions) => Promise<OnlyneBridge>;
  /** When set, health reports a local demo node and no live cluster is required. */
  demo?: boolean;
}

interface LiveRuntime {
  bot: Bot;
  conversation: Conversation;
  connection: Promise<RuntimeConnection>;
  calls: Map<string, AbortController>;
  earlySend?: (frame: RpcFrame) => void;
  dirty?: boolean;
  settled?: boolean;
  lastError?: string;
  awaitingSettle?: boolean;
}

export async function createDesktopApp(options: DesktopAppOptions): Promise<DesktopController> {
  const dataDir = resolve(options.dataDir);
  await mkdir(dataDir, { recursive: true });
  const store = options.store ?? await openDesktopStore(dataDir);
  const listeners = new Set<(event: DesktopEvent) => void>();
  const runtimes = new Map<string, LiveRuntime>();
  const drivers = new Map<string, RuntimeDriver>();
  const tools = new Map<string, HostTool>();
  const uris = new Map<string, HostUri>();
  const plugins: DesktopPlugin[] = [];
  const botExtensions = new Map<string, { paths: string; tools: Map<string, HostTool>; uris: Map<string, HostUri>; drivers: Map<string, RuntimeDriver>; plugins: DesktopPlugin[] }>();
  const views = new Map<string, WidgetEnvelope>();
  const answers = new Map<string, { resolve(value: unknown): void; reject(error: Error): void }>();
  let closed = false;
  const bridges = new Map<string, Promise<OnlyneBridge>>();
  const routers = new Map<string, RuntimeConnection>();
  const routerProfiles = new Map<string, string>();
  let decisionQueue: Promise<unknown> = Promise.resolve();
  const routerWaiters = new Map<string, { resolve(value: unknown): void; reject(error: Error): void; output: string }>();
  const turnScheduler = new GroupTurnScheduler();

  const emit = (event: DesktopEvent): void => {
    for (const listener of listeners) listener(event);
  };
  const snapshot = (): DesktopState => store.read();
  const defaultBot = (): Bot => {
    const bot = snapshot().bots[0];
    if (bot === undefined) throw new Error('The team has no bots');
    return bot;
  };
  const runtimeEntries = () => [...runtimes.values()].map(runtime => ({
    botId: runtime.bot.id, conversationId: runtime.conversation.id,
    settled: runtime.settled !== false, pendingCalls: runtime.calls.size,
    ...(runtime.lastError === undefined ? {} : { lastError: runtime.lastError }),
  }));
  const healthSummary = () => {
    const state = snapshot();
    return {
      bots: state.bots.length, conversations: state.conversations.length, messages: state.messages.length,
      media: state.media.length, runtimes: runtimeEntries(), bridges: bridges.size,
      teamAdminId: state.teamAdminId ?? null,
      demo: options.demo === true,
      errors: [...runtimes.values()].flatMap(runtime => runtime.lastError === undefined ? [] : [{ botId: runtime.bot.id, conversationId: runtime.conversation.id, message: runtime.lastError }]),
    };
  };
  const publish = (): void => emit({ type: 'snapshot', state: snapshot() });
  const save = async <T>(mutate: (state: DesktopState) => T): Promise<T> => {
    const result = await store.update(mutate);
    publish();
    return result;
  };
  const media = createMediaService(dataDir, () => snapshot().media, async item => {
    await save(state => { state.media.push(item); });
  });
  const botFor = (id: unknown): Bot => {
    const bot = snapshot().bots.find(item => item.id === id);
    if (bot === undefined) throw new Error(`Bot not found: ${String(id)}`);
    return bot;
  };
  const conversationFor = (id: unknown): Conversation => {
    const conversation = snapshot().conversations.find(item => item.id === id);
    if (conversation === undefined) throw new Error(`Conversation not found: ${String(id)}`);
    return conversation;
  };
  const keyFor = (botId: string, conversationId: string): string => `${botId}/${conversationId}`;
  const seedGreeting = async (bot: Bot, reset = false): Promise<void> => {
    const greeting = bot.character?.firstMessage;
    if (greeting === undefined || greeting.trim().length === 0) return;
    const direct = snapshot().conversations.find(item => item.kind === 'direct' && item.botIds.includes(bot.id));
    if (direct === undefined) return;
    const hasAssistant = snapshot().messages.some(item => item.conversationId === direct.id && item.role === 'assistant');
    if (hasAssistant && !reset) return;
    await save(state => {
      if (reset) state.messages = state.messages.filter(item => item.conversationId !== direct.id);
      state.messages.push({ id: shortId('msg'), conversationId: direct.id, role: 'assistant', botId: bot.id, text: greeting, createdAt: Date.now() });
      const target = state.conversations.find(item => item.id === direct.id);
      if (target !== undefined) { target.greeting = greeting; target.greeted = false; }
    });
  };
  const api: DesktopPluginApi = {
    registerRuntime(driver) {
      if (drivers.has(driver.id)) throw new Error(`Runtime already registered: ${driver.id}`);
      drivers.set(driver.id, driver);
    },
    registerTool(tool) {
      if (tools.has(tool.name)) throw new Error(`Host tool already registered: ${tool.name}`);
      tools.set(tool.name, tool);
    },
    registerUri(uri) {
      if (uris.has(uri.scheme)) throw new Error(`URI scheme already registered: ${uri.scheme}`);
      uris.set(uri.scheme, uri);
    },
    emit,
  };
  api.registerRuntime(ompRpcDriver);
  for (const driver of options.drivers ?? []) api.registerRuntime(driver);

  const loadBotPlugins = async (bot: Bot) => {
    const paths = JSON.stringify(bot.plugins?.host ?? []);
    const existing = botExtensions.get(bot.id);
    if (existing?.paths === paths) return existing;
    if (existing !== undefined && [...runtimes.values()].some(runtime => runtime.bot.id === bot.id && runtime.settled !== true)) return existing;
    if (existing !== undefined) for (const plugin of [...existing.plugins].reverse()) await plugin.dispose?.();
    const scoped = { paths, tools: new Map(tools), uris: new Map(uris), drivers: new Map(drivers), plugins: [] as DesktopPlugin[] };
    const scopedApi: DesktopPluginApi = {
      registerTool(tool) {
        if (scoped.tools.has(tool.name)) throw new Error(`Host tool already registered for ${bot.id}: ${tool.name}`);
        scoped.tools.set(tool.name, tool);
      },
      registerUri(uri) {
        if (scoped.uris.has(uri.scheme)) throw new Error(`URI scheme already registered for ${bot.id}: ${uri.scheme}`);
        scoped.uris.set(uri.scheme, uri);
      },
      registerRuntime(driver) {
        if (scoped.drivers.has(driver.id)) throw new Error(`Runtime already registered for ${bot.id}: ${driver.id}`);
        scoped.drivers.set(driver.id, driver);
      },
      emit: event => emit({ ...event, botId: bot.id }),
    };
    try {
      for (const path of bot.plugins?.host ?? []) {
        // Each bot chooses these module paths in its own .omp/bot.json.
        const module = await import(pathToFileURL(resolve(bot.configDir ?? botConfigDir(dataDir, bot.id), path)).href);
        if (typeof module.default !== 'function') throw new Error(`Host plugin must export a factory: ${path}`);
        const plugin: DesktopPlugin = await module.default({ bot, dataDir: join(dataDir, 'bots', bot.id) });
        if (plugin === undefined || typeof plugin.setup !== 'function') throw new Error(`Host plugin factory returned an invalid plugin: ${path}`);
        await plugin.setup(scopedApi);
        scoped.plugins.push(plugin);
      }
    } catch (error) {
      for (const plugin of [...scoped.plugins].reverse()) await plugin.dispose?.();
      throw error;
    }
    botExtensions.set(bot.id, scoped);
    return scoped;
  };

  const contextFor = (runtime: LiveRuntime, controller: AbortController, update: HostToolContext['update']): HostToolContext => ({
    bot: runtime.bot, conversation: runtime.conversation, signal: controller.signal, update,
  });

  const runHostCall = async (runtime: LiveRuntime, frame: RpcFrame): Promise<void> => {
    const connection = await runtime.connection;
    const id = text(frame.id, 'request id');
    const controller = new AbortController();
    runtime.calls.set(id, controller);
    const context = contextFor(runtime, controller, partialResult => connection.send({ type: 'host_tool_update', id, partialResult }));
    try {
      if (frame.type === 'host_tool_call') {
        const tool = botExtensions.get(runtime.bot.id)?.tools.get(String(frame.toolName));
        if (tool === undefined) throw new Error(`Unknown host tool: ${String(frame.toolName)}`);
        const result = await tool.execute(record(frame.arguments), context);
        connection.send({ type: 'host_tool_result', id, result });
      } else {
        const url = text(frame.url, 'url');
        const scheme = url.slice(0, url.indexOf(':'));
        const uri = botExtensions.get(runtime.bot.id)?.uris.get(scheme);
        if (uri === undefined) throw new Error(`Unknown URI scheme: ${scheme}`);
        if (frame.operation === 'write') {
          if (!uri.writable || uri.write === undefined) throw new Error(`URI scheme is read-only: ${scheme}`);
          await uri.write(url, typeof frame.content === 'string' ? frame.content : '', context);
          connection.send({ type: 'host_uri_result', id });
        } else {
          const result = await uri.read(url, context);
          connection.send({ type: 'host_uri_result', id, ...result });
        }
      }
    } catch (error) {
      if (frame.type === 'host_tool_call') {
        connection.send({ type: 'host_tool_result', id, isError: true, result: toolResult({ error: errorText(error) }) });
      } else connection.send({ type: 'host_uri_result', id, isError: true, error: errorText(error) });
    } finally { runtime.calls.delete(id); }
  };

  const onFrame = (runtime: LiveRuntime, frame: RpcFrame): void => {
    const { bot, conversation } = runtime;
    emit({ type: 'rpc', botId: bot.id, conversationId: conversation.id, frame });
    if (frame.type === 'host_tool_call' || frame.type === 'host_uri_request') {
      void runHostCall(runtime, frame).catch(error => emit({ type: 'error', message: errorText(error), botId: bot.id, conversationId: conversation.id }));
    }
    if (frame.type === 'host_tool_cancel' || frame.type === 'host_uri_cancel') runtime.calls.get(String(frame.targetId))?.abort();
    if (frame.type === 'agent_start') emit({ type: 'runtime', botId: bot.id, conversationId: conversation.id, status: 'running' });
    if (frame.type === 'agent_start') runtime.settled = false;
    if (frame.type === 'session_settled') {
      runtime.settled = true;
      emit({ type: 'runtime', botId: bot.id, conversationId: conversation.id, status: 'idle' });
      if (runtime.awaitingSettle === true) {
        runtime.awaitingSettle = false;
        void autoAdvance(conversation.id, bot.id);
      }
    }
    if (frame.type === 'prompt_result' && frame.status === 'error') {
      const error = frame.error as { message?: string } | undefined;
      runtime.lastError = error?.message ?? 'OMP prompt failed';
      runtime.awaitingSettle = false;
      turnScheduler.pause(conversation.id);
      emit({ type: 'auto', conversationId: conversation.id, active: false, turns: 0, reason: 'error' });
      emit({ type: 'error', botId: bot.id, conversationId: conversation.id, message: error?.message ?? 'OMP prompt failed' });
    }
    if (frame.type === 'prompt_result' && frame.status === 'completed') {
      runtime.lastError = undefined;
      runtime.awaitingSettle = true;
    }
    if (frame.type === 'runtime_exit') {
      runtimes.delete(keyFor(bot.id, conversation.id));
      for (const controller of runtime.calls.values()) controller.abort();
      emit({ type: 'runtime', botId: bot.id, conversationId: conversation.id, status: 'stopped' });
    }
    if (frame.type === 'message_end' && frame.message !== null && typeof frame.message === 'object') {
      const message = frame.message as Record<string, unknown>;
      const body = contentText(message.content);
      if (message.role !== 'assistant' || body.length === 0) return;
      const hash = createHash('sha256').update(JSON.stringify(message)).digest('hex').slice(0, 24);
      const item: ChatMessage = {
        id: `${bot.id}-${conversation.id}-${hash}`, conversationId: conversation.id, botId: bot.id,
        role: 'assistant', text: body, content: message.content as Json[],
        createdAt: typeof message.timestamp === 'number' ? message.timestamp : Date.now(),
        ...(message.usage !== null && typeof message.usage === 'object' ? { usage: message.usage as Record<string, number> } : {}),
      };
      void save(state => {
        if (!state.messages.some(entry => entry.id === item.id)) state.messages.push(item);
      }).catch(error => emit({ type: 'error', message: errorText(error) }));
    }
  };

  const getRuntime = async (bot: Bot, conversation: Conversation): Promise<RuntimeConnection> => {
    if (closed) throw new Error('Desktop app is closed');
    if (!conversation.botIds.includes(bot.id)) throw new Error('Bot is not a member of this conversation');
    const key = keyFor(bot.id, conversation.id);
    const existing = runtimes.get(key);
    if (existing !== undefined) {
      if (!existing.dirty || existing.settled === false) return existing.connection;
      await stopRuntime(bot.id, conversation.id);
    }
    const extensions = await loadBotPlugins(bot);
    const driver = extensions.drivers.get(bot.runtime);
    if (driver === undefined) throw new Error(`Runtime driver is not registered: ${bot.runtime}`);
    const runtime = { bot, conversation, calls: new Map(), settled: false } as LiveRuntime;
    const state = snapshot();
    runtime.connection = (async () => {
      const connection = await driver.connect({
        bot, conversation, capabilities: state.capabilities.filter(item => bot.capabilities.includes(item.id)),
        dataDir, sessionDir: join(dataDir, 'sessions', bot.id, conversation.id),
        onFrame: frame => onFrame(runtime, frame),
        onEarlySend: send => { runtime.earlySend = send; },
      });
      await connection.request({ type: 'set_host_tools', tools: [...extensions.tools.values()].map(({ execute: _execute, ...definition }) => definition) });
      await connection.request({ type: 'set_host_uri_schemes', schemes: [...extensions.uris.values()].map(({ read: _read, write: _write, ...definition }) => definition) });
      runtime.settled = true;
      return connection;
    })();
    runtimes.set(key, runtime);
    try { return await runtime.connection; }
    catch (error) { runtimes.delete(key); throw error; }
  };

  const stopRuntime = async (botId: string, conversationId: string): Promise<void> => {
    const key = keyFor(botId, conversationId);
    const runtime = runtimes.get(key);
    if (runtime === undefined) return;
    runtimes.delete(key);
    for (const controller of runtime.calls.values()) controller.abort();
    const connection = await runtime.connection;
    await connection.close();
  };

  const prompt = async (bot: Bot, conversation: Conversation, message: string, userText?: string): Promise<void> => {
    if (options.demo === true) {
      const spoken = (userText ?? '').replace(/\s+/g, ' ').trim();
      if (spoken === '/fail' || spoken.startsWith('/fail ')) {
        emit({ type: 'error', botId: bot.id, conversationId: conversation.id, message: 'Executable not found in $PATH: "omp"' });
        return;
      }
      emit({ type: 'typing', conversationId: conversation.id, botId: bot.id, demo: true });
      await new Promise(resolve => setTimeout(resolve, 400));
      const text = demoLine(bot, spoken);
      await save(state => {
        state.messages.push({ id: shortId('msg'), conversationId: conversation.id, role: 'assistant', botId: bot.id, text, createdAt: Date.now() });
      });
      emit({ type: 'typing', conversationId: conversation.id, botId: bot.id, demo: true, done: true });
      return;
    }
    try {
      const connection = await getRuntime(bot, conversation);
      await connection.request({ type: 'prompt', message, streamingBehavior: 'followUp' });
    } catch (error) { emit({ type: 'error', botId: bot.id, conversationId: conversation.id, message: errorText(error) }); }
  };

  /**
   * One automated group turn per completed reply. The counter resets on any user message,
   * so a group scene cannot talk forever without a person; the ceiling is explicit.
   */
  const autoAdvance = async (conversationId: string, fromBotId: string): Promise<void> => {
    const conversation = snapshot().conversations.find(item => item.id === conversationId);
    if (conversation === undefined) return;
    if ([...runtimes.values()].some(runtime => runtime.conversation.id === conversationId && runtime.settled !== true)) return;
    if (turnScheduler.isActive(conversationId)) return;
    const decision = turnScheduler.decide(conversation, fromBotId);
    if (decision.kind === 'idle') return;
    if (decision.kind === 'limit') {
      emit({ type: 'auto', conversationId, active: false, turns: decision.turn, max: decision.max, reason: 'limit' });
      return;
    }
    const next = snapshot().bots.find(item => item.id === decision.botId);
    if (next === undefined || !turnScheduler.begin(conversationId, decision.turn)) return;
    try {
      const transcript = snapshot().messages.filter(item => item.conversationId === conversationId);
      const compiled = compileScene(next, conversation, transcript, 'Continue the scene as your character, answering the last message in the transcript. Keep it brief unless the scene needs more.', { includeHistory: true, members: conversation.botIds.map(botFor) });
      emit({ type: 'auto', conversationId, active: true, turns: decision.turn, max: decision.max, botId: next.id });
      await prompt(next, conversation, compiled.prompt);
    } catch (error) {
      turnScheduler.pause(conversationId);
      emit({ type: 'auto', conversationId, active: false, turns: 0, reason: 'error' });
      emit({ type: 'error', conversationId, message: errorText(error) });
    } finally {
      turnScheduler.end(conversationId);
    }
  };

  const bridgeFor = async (bot: Bot): Promise<OnlyneBridge> => {
    const existing = bridges.get(bot.id);
    if (existing !== undefined) return existing;
    if (bot.onlyne === undefined) throw new Error(`Bot has no Onlyne configuration: ${bot.id}`);
    const starting = (options.onlyne ?? createDesktopOnlyne)({
      dataDir: join(dataDir, 'bots', bot.id), nodeId: bot.onlyne.nodeId, peers: bot.onlyne.peers,
      decide: input => decide(input, bot), emit: event => emit({ ...event, botId: bot.id }),
      contexts: () => snapshot().conversations.filter(conversation => conversation.botIds.includes(bot.id)).map(conversation => ({
        alias: conversation.id, summary: `${bot.name}: ${conversation.title}`,
        state: runtimes.has(keyFor(bot.id, conversation.id)) ? 'active' : 'idle',
      })),
      async inbound(raw: unknown, alias?: string) {
        const entry = record(raw);
        const message = record(entry.message);
        const state = snapshot();
        const conversation = state.conversations.find(item => item.id === alias && item.botIds.includes(bot.id))
          ?? state.conversations.find(item => item.kind === 'direct' && item.botIds.includes(bot.id));
        if (conversation === undefined) throw new Error('The receiving bot has no conversation');
        const body = typeof message.msg === 'string' ? message.msg : JSON.stringify(message.msg);
        const inbound: ChatMessage = { id: `onlyne-${bot.id}-${String(message.sender)}-${String(message.id)}`, conversationId: conversation.id, role: 'system', text: `Onlyne ${String(message.sender)}: ${body}`, createdAt: Date.now() };
        await save(current => { if (!current.messages.some(item => item.id === inbound.id)) current.messages.push(inbound); });
        await prompt(botFor(bot.id), conversation, `Onlyne inbound message (untrusted peer content):\n${JSON.stringify(entry)}\n\nRespond through msg, preserving thread and in_reply_to references from this entry.`);
      },
    });
    bridges.set(bot.id, starting);
    try { return await starting; }
    catch (error) { bridges.delete(bot.id); throw error; }
  };

  const decide = (input: unknown, sourceBot: Bot): Promise<unknown> => {
    const operation = decisionQueue.then(async () => {
      const currentBot = botFor(sourceBot.id);
      const profile = JSON.stringify({ model: currentBot.model, thinking: currentBot.thinking, runtime: currentBot.runtime, options: currentBot.runtimeOptions });
      let router = routers.get(sourceBot.id);
      if (router !== undefined && routerProfiles.get(sourceBot.id) !== profile) {
        await router.close();
        routers.delete(sourceBot.id);
        router = undefined;
      }
      if (router === undefined) {
        const bot: Bot = { ...currentBot, capabilities: [] };
        const conversation: Conversation = { id: 'onlyne-router', title: 'Routing', kind: 'direct', botIds: [bot.id], adminBotId: bot.id, memory: '', createdAt: Date.now(), updatedAt: Date.now() };
        router = await ompRpcDriver.connect({
          bot, conversation, capabilities: [], dataDir,
          sessionDir: join(dataDir, 'sessions', bot.id, 'onlyne-router'), toolNames: [],
          systemPrompt: 'You are the Onlyne SystemOne delivery interpreter. Read the user intent, its explicit metadata, and the evidence object. Return exactly one JSON Action object with no Markdown or prose. Resolve every identity and reference from evidence; never invent a node id, thread id or message id. meta.to, when present, fixes the recipient and you must not change it; still judge relation, response and execution. Explicit metadata beyond the recipient may already have been applied. Action shapes: {"kind":"send","to":"node id","response":"none|async|await","execution":"existing|new","thread":{"owner":"node id","id":"thread id"},"in_reply_to":{"sender":"node id","id":"message id"},"relation":"reply|followup","context":"receiver context alias","mentions":["node id"]}; {"kind":"read","scope":"directory|neighbors|threads|inbox|messages|message|evidence","thread":{"owner":"node id","id":"thread id"},"message":{"sender":"node id","id":"message id"},"seq":"entry sequence","attention":"inbox|thread","page_size":50,"cursor":"opaque"}; {"kind":"read","scope":"resources","query":{"op":"list","owners":["node id"],"page_size":50}}; {"kind":"thread","op":"create|close|list|describe|member_add|member_remove","thread":{"owner":"node id","id":"thread id"},"to":"member node id","title":"thread title"}; {"kind":"domain","op":"create|join|leave|list|describe|discover","domain_id":"id","expected_revision":"revision"}; {"kind":"resource","resource":{"owner":"node id","id":"resource id"},"operation":"operation name","data":{},"expected_revision":"revision"}. Omit optional fields you cannot support with evidence. A send intent stays a send even when its text mentions files or tools. For an unknown or ambiguous reference return {"error":"the specific missing or ambiguous identity"}.',
          onFrame(frame) {
            if (frame.type === 'message_end') {
              const output = frame.message as Record<string, unknown> | undefined;
              if (output?.role === 'assistant') for (const [key, waiter] of routerWaiters) if (key.startsWith(`${sourceBot.id}/`)) waiter.output = contentText(output.content);
            }
            if (frame.type === 'prompt_result' && typeof frame.id === 'string') {
              const key = `${sourceBot.id}/${frame.id}`;
              const pendingKey = `${sourceBot.id}/pending`;
              const waiter = routerWaiters.get(key) ?? routerWaiters.get(pendingKey);
              if (waiter === undefined) return;
              routerWaiters.delete(key);
              routerWaiters.delete(pendingKey);
              if (frame.status !== 'completed') waiter.reject(new Error(`Onlyne decision failed: ${JSON.stringify(frame.error ?? frame.status)}`));
              else {
                try {
                  const result = record(JSON.parse(waiter.output));
                  if (typeof result.error === 'string') throw new Error(result.error);
                  waiter.resolve(result);
                } catch (error) { waiter.reject(error instanceof Error ? error : new Error(String(error))); }
              }
            }
            if (frame.type === 'runtime_error' || frame.type === 'runtime_exit') {
              for (const [key, waiter] of routerWaiters) if (key.startsWith(`${sourceBot.id}/`)) {
                waiter.reject(new Error(String(frame.error ?? 'Routing runtime exited')));
                routerWaiters.delete(key);
              }
              routers.delete(sourceBot.id);
            }
          },
        });
        routers.set(sourceBot.id, router);
        routerProfiles.set(sourceBot.id, profile);
      }
      const connection = router;
      await connection.request({ type: 'new_session' });
      const { promise, resolve: resolveDecision, reject } = Promise.withResolvers<unknown>();
      // Subscribe before sending: a local prompt can complete before its acknowledgement returns.
      const pending = { resolve: resolveDecision, reject, output: '' };
      const pendingKey = `${sourceBot.id}/pending`;
      routerWaiters.set(pendingKey, pending);
      const response = await connection.request({ type: 'prompt', message: JSON.stringify(input) });
      if (response.data !== null && typeof response.data === 'object' && 'agentInvoked' in response.data && response.data.agentInvoked === false) {
        routerWaiters.delete(pendingKey);
        throw new Error('Routing prompt did not invoke the decision model');
      }
      if (typeof response.id !== 'string') throw new Error('Routing response has no request id');
      if (routerWaiters.delete(pendingKey)) routerWaiters.set(`${sourceBot.id}/${response.id}`, pending);
      return promise;
    });
    decisionQueue = operation.catch(() => undefined);
    return operation;
  };

  const controller: DesktopController = {
    snapshot,
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    async command(action, payload) {
      if (closed) throw new Error('Desktop app is closed');
      switch (action) {
        case 'bot.save': {
          const value = record(payload.bot);
          const old = value.id === undefined ? undefined : snapshot().bots.find(item => item.id === value.id);
          const id = old?.id ?? (value.id === undefined ? shortId('bot') : identifier(value.id));
          const now = Date.now();
          const bot: Bot = {
            id, name: text(value.name ?? old?.name, 'name'),
            description: typeof value.description === 'string' ? value.description : old?.description ?? '',
            instructions: typeof value.instructions === 'string' ? value.instructions : old?.instructions ?? '',
            memory: typeof value.memory === 'string' ? value.memory : old?.memory ?? '',
            workspace: resolve(typeof value.workspace === 'string' && value.workspace.trim() ? value.workspace : old?.workspace ?? join(dataDir, 'workspaces', id)),
            thinking: (value.thinking ?? old?.thinking ?? 'inherit') as Bot['thinking'],
            capabilities: Array.isArray(value.capabilities) ? value.capabilities.map(item => text(item, 'capability')) : old?.capabilities ?? [],
            runtime: typeof value.runtime === 'string' ? value.runtime : old?.runtime ?? 'omp-rpc',
            runtimeOptions: value.runtimeOptions === undefined ? old?.runtimeOptions ?? {} : record(value.runtimeOptions),
            configDir: botConfigDir(dataDir, id),
            onlyne: value.onlyne === undefined ? old?.onlyne : {
              nodeId: text(record(value.onlyne).nodeId, 'Onlyne node id'),
              peers: Array.isArray(record(value.onlyne).peers) ? (record(value.onlyne).peers as unknown[]).map(peer => text(peer, 'peer address')) : [],
            },
            plugins: value.plugins === undefined ? old?.plugins ?? { host: [], renderer: [] } : {
              host: Array.isArray(record(value.plugins).host) ? (record(value.plugins).host as unknown[]).map(path => text(path, 'host plugin path')) : [],
              renderer: Array.isArray(record(value.plugins).renderer) ? (record(value.plugins).renderer as unknown[]).map(path => text(path, 'renderer plugin path')) : [],
            },
            character: value.character === undefined ? old?.character : normalizeCharacter(value.character),
            lorebook: value.lorebook === undefined ? old?.lorebook : parseLorebook(value.lorebook),
            ...(value.model === null ? {} : value.model === undefined ? old?.model === undefined ? {} : { model: old.model } : { model: { provider: text(record(value.model).provider, 'model provider'), id: text(record(value.model).id, 'model id') } }),
            createdAt: old?.createdAt ?? now, updatedAt: now,
          };
          if (!drivers.has(bot.runtime) && (bot.plugins?.host.length ?? 0) === 0) throw new Error(`Unknown runtime driver: ${bot.runtime}`);
          await mkdir(bot.workspace, { recursive: true });
          const configuredBot = await saveBotConfiguration(dataDir, bot);
          for (const runtime of runtimes.values()) if (runtime.bot.id === id) { runtime.bot = configuredBot; runtime.dirty = true; }
          const saved = await save(state => {
            const position = state.bots.findIndex(item => item.id === id);
            if (position < 0) state.bots.push(configuredBot); else state.bots[position] = configuredBot;
            if (!state.conversations.some(item => item.kind === 'direct' && item.botIds.includes(id))) state.conversations.push({ id: shortId('c'), title: bot.name, kind: 'direct', botIds: [id], adminBotId: id, memory: '', createdAt: now, updatedAt: now });
            return configuredBot;
          });
          await seedGreeting(configuredBot);
          if (old?.onlyne !== undefined && JSON.stringify(old.onlyne) !== JSON.stringify(configuredBot.onlyne)) {
            const previous = bridges.get(id);
            bridges.delete(id);
            await (await previous)?.close();
          }
          await bridgeFor(configuredBot);
          return saved;
        }
        case 'bot.reload': {
          const previous = botFor(payload.id);
          const bot = await loadBotConfiguration(dataDir, previous);
          for (const runtime of runtimes.values()) if (runtime.bot.id === bot.id) { runtime.bot = bot; runtime.dirty = true; }
          await save(state => { state.bots = state.bots.map(item => item.id === bot.id ? bot : item); });
          if (JSON.stringify(previous.onlyne) !== JSON.stringify(bot.onlyne)) {
            const bridge = bridges.get(bot.id);
            bridges.delete(bot.id);
            await (await bridge)?.close();
            await bridgeFor(bot);
          }
          return bot;
        }
        case 'bot.delete': {
          const bot = botFor(payload.id);
          if (bot.id === snapshot().teamAdminId) throw new Error('Transfer team administration before deleting this bot');
          if (snapshot().bots.length === 1) throw new Error('Keep at least one bot');
          for (const runtime of [...runtimes.values()]) if (runtime.bot.id === bot.id) await stopRuntime(bot.id, runtime.conversation.id);
          const bridge = bridges.get(bot.id);
          bridges.delete(bot.id);
          await (await bridge)?.close();
          return save(state => {
            state.bots = state.bots.filter(item => item.id !== bot.id);
            state.conversations = state.conversations.filter(item => item.kind !== 'direct' || !item.botIds.includes(bot.id));
            for (const conversation of state.conversations) {
              conversation.botIds = conversation.botIds.filter(id => id !== bot.id);
              if (!conversation.botIds.includes(conversation.adminBotId)) conversation.adminBotId = conversation.botIds[0] ?? '';
            }
            state.conversations = state.conversations.filter(item => item.botIds.length > 0);
            return { deleted: bot.id };
          });
        }
        case 'conversation.create':
        case 'conversation.update': {
          const old = action === 'conversation.update' ? conversationFor(payload.id) : undefined;
          const ids = Array.isArray(payload.botIds) ? payload.botIds.map(id => botFor(id).id) : old?.botIds ?? [];
          const botIds = [...new Set(ids)];
          if (botIds.length === 0) throw new Error('A conversation needs at least one bot');
          const adminBotId = typeof payload.adminBotId === 'string' ? payload.adminBotId : old?.adminBotId ?? botIds[0]!;
          if (!botIds.includes(adminBotId)) throw new Error('The admin must be a conversation member');
          const kind = payload.kind ?? old?.kind ?? 'group';
          if (kind !== 'direct' && kind !== 'group') throw new Error('Unknown conversation kind');
          if (kind === 'direct' && botIds.length !== 1) throw new Error('A direct conversation has exactly one bot');
          const conversation: Conversation = {
            id: old?.id ?? shortId('c'), title: text(payload.title ?? old?.title, 'title'), kind, botIds, adminBotId,
            memory: typeof payload.memory === 'string' ? payload.memory : old?.memory ?? '',
            ...(typeof payload.personaName === 'string' ? { personaName: payload.personaName } : old?.personaName === undefined ? {} : { personaName: old.personaName }),
            ...(typeof payload.personaDescription === 'string' ? { personaDescription: payload.personaDescription } : old?.personaDescription === undefined ? {} : { personaDescription: old.personaDescription }),
            ...(typeof payload.authorNote === 'string' ? { authorNote: payload.authorNote } : old?.authorNote === undefined ? {} : { authorNote: old.authorNote }),
            ...(typeof payload.greeting === 'string' ? { greeting: payload.greeting } : old?.greeting === undefined ? {} : { greeting: old.greeting }),
            ...(typeof payload.greeted === 'boolean' ? { greeted: payload.greeted } : old?.greeted === undefined ? {} : { greeted: old.greeted }),
            ...(payload.scene === undefined ? old?.scene === undefined ? {} : { scene: old.scene } : { scene: record(payload.scene) as Conversation['scene'] }),
            createdAt: old?.createdAt ?? Date.now(), updatedAt: Date.now(),
          };
          turnScheduler.reset(conversation.id);
          emit({ type: 'auto', conversationId: conversation.id, active: false, turns: 0, max: conversation.scene?.group?.maxAutoTurns ?? DEFAULT_MAX_AUTO_TURNS, reason: 'scene-changed' });
          if (old !== undefined) for (const runtime of runtimes.values()) if (runtime.conversation.id === old.id) { runtime.conversation = conversation; runtime.dirty = true; }
          return save(state => {
            const position = state.conversations.findIndex(item => item.id === conversation.id);
            if (position < 0) state.conversations.push(conversation); else state.conversations[position] = conversation;
            return conversation;
          });
        }
        case 'message.send': {
          const conversation = conversationFor(payload.conversationId);
          turnScheduler.reset(conversation.id);
          emit({ type: 'auto', conversationId: conversation.id, active: false, turns: 0, max: conversation.scene?.group?.maxAutoTurns ?? DEFAULT_MAX_AUTO_TURNS, reason: 'user-message' });
          const body = typeof payload.text === 'string' ? payload.text.trim() : '';
          let selected = Array.isArray(payload.botIds) ? payload.botIds.map(id => botFor(id).id) : [];
          if (selected.length === 0) {
            const members = conversation.botIds.map(botFor);
            selected = /@(?:all|everyone|所有人)(?:\s|$)/i.test(body) ? conversation.botIds : members.filter(bot => body.includes(`@${bot.name}`) || body.includes(`@${bot.id}`)).map(bot => bot.id);
          }
          if (selected.length === 0 && conversation.scene?.group?.mode === 'round-robin') {
            const order = (conversation.scene.group.order ?? conversation.botIds).filter(id => conversation.botIds.includes(id));
            const last = [...snapshot().messages].reverse().find(item => item.conversationId === conversation.id && item.role === 'assistant' && item.botId !== undefined);
            const position = last?.botId === undefined ? -1 : order.indexOf(last.botId);
            selected = [order[(position + 1) % Math.max(order.length, 1)] ?? conversation.adminBotId];
          }
          if (selected.length === 0) selected = [conversation.adminBotId];
          if (selected.some(id => !conversation.botIds.includes(id))) throw new Error('Selected bot is not a conversation member');
          const attachments = Array.isArray(payload.mediaIds)
            ? payload.mediaIds.map(id => { const item = media.item(text(id, 'media id')); if (item === undefined) throw new Error(`Media not found: ${String(id)}`); return item; })
            : [];
          if (body.length === 0 && attachments.length === 0) throw new Error('A message needs text or media');
          const message: ChatMessage = { id: shortId('msg'), conversationId: conversation.id, role: 'user', text: body, ...(attachments.length === 0 ? {} : { media: attachments }), createdAt: Date.now() };
          await save(state => { state.messages.push(message); });
          const transcript = snapshot().messages.filter(item => item.conversationId === conversation.id && item.id !== message.id);
          for (const id of new Set(selected)) {
            const compiled = compileScene(botFor(id), conversation, transcript, body, {
              includeHistory: conversation.kind === 'group',
              members: conversation.botIds.map(botFor),
            });
            void prompt(botFor(id), conversation, compiled.prompt, body);
          }
          if (conversation.greeting !== undefined && conversation.greeted !== true) {
            await save(state => { const target = state.conversations.find(item => item.id === conversation.id); if (target !== undefined) target.greeted = true; });
          }
          return message;
        }
        case 'message.retry': {
          const conversation = conversationFor(payload.conversationId);
          const last = [...snapshot().messages].reverse().find(item => item.conversationId === conversation.id && item.role === 'user');
          if (last === undefined) throw new Error('There is no message to send again.');
          const bot = botFor(payload.botId ?? conversation.adminBotId);
          if (!conversation.botIds.includes(bot.id)) throw new Error('Bot is not a member of this conversation');
          const transcript = snapshot().messages.filter(item => item.conversationId === conversation.id && item.id !== last.id);
          const compiled = compileScene(bot, conversation, transcript, last.text, { includeHistory: conversation.kind === 'group', members: conversation.botIds.map(botFor) });
          await prompt(bot, conversation, compiled.prompt, last.text);
          return { retried: last.id };
        }
        case 'message.regenerate': {
          const conversation = conversationFor(payload.conversationId);
          const bot = botFor(payload.botId ?? conversation.adminBotId);
          if (!conversation.botIds.includes(bot.id)) throw new Error('Bot is not a member of this conversation');
          const state = snapshot();
          const last = [...state.messages].reverse().find(item => item.conversationId === conversation.id && item.role === 'assistant' && item.botId === bot.id);
          if (last === undefined) throw new Error('This bot has no assistant message to regenerate');
          await save(current => { current.messages = current.messages.filter(item => item.id !== last.id); });
          const transcript = snapshot().messages.filter(item => item.conversationId === conversation.id);
          const compiled = compileScene(bot, conversation, transcript, 'Regenerate your previous reply with different wording and a different action. Do not repeat it.', { includeHistory: true });
          await prompt(bot, conversation, compiled.prompt, 'Please write that again, in different words.');
          return { removed: last.id, state: compiled.stats };
        }
        case 'harness.preview': {
          const conversation = conversationFor(payload.conversationId);
          const bot = botFor(payload.botId ?? conversation.adminBotId);
          const draft = typeof payload.text === 'string' ? payload.text : 'Preview the next turn.';
          const compiled = compileScene(bot, conversation, snapshot().messages.filter(item => item.conversationId === conversation.id), draft, {
            includeHistory: conversation.kind === 'group', members: conversation.botIds.map(botFor),
          });
          return compiled;
        }
        case 'media.upload': {
          const bytes = Buffer.from(text(payload.content, 'media content'), 'base64');
          const item = await media.put(new Uint8Array(bytes), {
            name: text(payload.name, 'media name'), contentType: text(payload.contentType, 'media content type'),
            ...(typeof payload.botId === 'string' ? { botId: payload.botId } : {}),
          });
          return item;
        }
        case 'media.list': {
          const items = media.list().filter(item => payload.botId === undefined || item.botId === payload.botId);
          return { items: items.slice(-(typeof payload.limit === 'number' ? payload.limit : 50)) };
        }
        case 'sticker.list': {
          const bot = botFor(payload.botId);
          const directory = join(dataDir, 'bots', bot.id, '.omp', 'stickers');
          const entries = await readdir(directory).catch(() => []);
          return { names: entries.filter(name => /\.(png|gif|webp|jpg|jpeg)$/i.test(name)).map(name => name.replace(/\.[^.]+$/, '')).sort() };
        }
        case 'sticker.send': {
          const conversation = conversationFor(payload.conversationId);
          const bot = botFor(payload.botId ?? conversation.botIds[0]);
          const item = await media.fromPath(stickerPath(dataDir, bot.id, text(payload.name, 'sticker name')), { botId: bot.id, name: text(payload.name, 'sticker name'), kind: 'sticker' });
          const message: ChatMessage = { id: shortId('msg'), conversationId: conversation.id, role: 'user', text: '', media: [item], createdAt: Date.now() };
          await save(state => { state.messages.push(message); });
          return message;
        }
        case 'character.import': {
          const bot = botFor(payload.botId);
          const bytes = typeof payload.content === 'string' && payload.content.length > 0
            ? Buffer.from(payload.content, payload.encoding === 'base64' ? 'base64' : 'utf8')
            : await readFile(text(payload.path, 'path'));
          if (bytes.length > 16 * 1024 * 1024) throw new Error('The character file is larger than 16 MB');
          const parsed = bytes[0] === 0x89 ? parsePngCharacter(bytes) : parseCharacterJson(JSON.parse(bytes.toString('utf8')));
          let avatar: string | undefined;
          if (bytes[0] === 0x89) {
            const item = await media.put(new Uint8Array(bytes), { name: `${bot.id}-avatar.png`, contentType: 'image/png', botId: bot.id });
            avatar = item.id;
          }
          const updated: Bot = { ...bot, character: { ...parsed.character, ...(avatar === undefined ? {} : { avatar }) }, ...(parsed.lorebook === undefined ? {} : { lorebook: parsed.lorebook }), updatedAt: Date.now() };
          const configured = await saveBotConfiguration(dataDir, updated);
          await save(state => { state.bots = state.bots.map(item => item.id === bot.id ? configured : item); });
          for (const runtime of runtimes.values()) if (runtime.bot.id === bot.id) { runtime.bot = configured; runtime.dirty = true; }
          await seedGreeting(configured);
          return { character: parsed.character, lorebook: parsed.lorebook ?? bot.lorebook };
        }
        case 'character.export': {
          const bot = botFor(payload.botId);
          return { name: bot.name, card: characterToJson(bot.name, bot.character ?? { description: bot.description }, bot.lorebook) };
        }
        case 'conversation.greet': {
          const conversation = conversationFor(payload.conversationId);
          const bot = botFor(payload.botId ?? conversation.botIds[0]);
          const card = bot.character;
          if (card === undefined) throw new Error(`${bot.name} has no character card`);
          const index = typeof payload.index === 'number' ? payload.index : -1;
          const greeting = index >= 0 ? card.alternateGreetings?.[index] : card.firstMessage;
          if (greeting === undefined || greeting.trim().length === 0) throw new Error('That greeting is empty');
          const message: ChatMessage = { id: shortId('msg'), conversationId: conversation.id, role: 'assistant', botId: bot.id, text: greeting, createdAt: Date.now() };
          await save(state => {
            if (payload.reset === true) state.messages = state.messages.filter(item => item.conversationId !== conversation.id);
            state.messages.push(message);
            const target = state.conversations.find(item => item.id === conversation.id);
            if (target !== undefined) { target.greeting = greeting; target.greeted = false; }
          });
          return message;
        }
        case 'runtime.command': {
          const connection = await getRuntime(botFor(payload.botId), conversationFor(payload.conversationId));
          return connection.request(record(payload.command) as RpcFrame);
        }
        case 'runtime.respond': {
          const bot = botFor(payload.botId);
          const conversation = conversationFor(payload.conversationId);
          const response = record(payload.response) as RpcFrame;
          if (response.type !== 'extension_ui_response') throw new Error('Expected an extension_ui_response');
          const runtime = runtimes.get(keyFor(bot.id, conversation.id));
          if (runtime === undefined) throw new Error('This conversation has no runtime');
          (runtime.earlySend ?? (await runtime.connection).send)(response);
          return { sent: true };
        }
        case 'runtime.stop': {
          const bot = botFor(payload.botId);
          const conversation = conversationFor(payload.conversationId);
          turnScheduler.pause(conversation.id);
          emit({ type: 'auto', conversationId: conversation.id, active: false, turns: 0, max: conversation.scene?.group?.maxAutoTurns ?? DEFAULT_MAX_AUTO_TURNS, reason: 'stopped' });
          const runtime = runtimes.get(keyFor(bot.id, conversation.id));
          if (runtime !== undefined) await (await runtime.connection).request({ type: 'abort_and_restore_queue' });
          return { stopped: true };
        }
        case 'runtime.restart': {
          const bot = botFor(payload.botId);
          const conversation = conversationFor(payload.conversationId);
          turnScheduler.pause(conversation.id);
          emit({ type: 'auto', conversationId: conversation.id, active: false, turns: 0, max: conversation.scene?.group?.maxAutoTurns ?? DEFAULT_MAX_AUTO_TURNS, reason: 'stopped' });
          await stopRuntime(bot.id, conversation.id);
          emit({ type: 'runtime', botId: bot.id, conversationId: conversation.id, status: 'stopped' });
          return { restarted: true, next: 'the next prompt starts a fresh runtime' };
        }
        case 'runtime.health': {
          return {
            runtimes: runtimeEntries(),
            autoTurns: turnScheduler.counts(),
          };
        }
        case 'desktop.health': {
          return healthSummary();
        }
        case 'team.admin': {
          const bot = botFor(payload.botId);
          return save(state => {
            state.teamAdminId = bot.id;
            return { teamAdminId: bot.id };
          });
        }
        case 'desktop.read': {
          const uri = text(payload.uri, 'uri');
          const scheme = uri.slice(0, uri.indexOf(':'));
          const handler = uris.get(scheme);
          if (handler === undefined) throw new Error(`Unknown URI scheme: ${scheme}`);
          const bot = payload.botId === undefined ? defaultBot() : botFor(payload.botId);
          const conversation = payload.conversationId === undefined
            ? snapshot().conversations.find(item => item.botIds.includes(bot.id))
            : conversationFor(payload.conversationId);
          if (conversation === undefined) throw new Error(`Bot ${bot.id} has no conversation`);
          return handler.read(uri, { bot, conversation, signal: new AbortController().signal, update() {} });
        }
        case 'runtime.models': {
          const bot = payload.botId === undefined ? snapshot().bots[0]! : botFor(payload.botId);
          const conversation = payload.conversationId === undefined ? snapshot().conversations.find(item => item.botIds.includes(bot.id))! : conversationFor(payload.conversationId);
          const connection = await getRuntime(bot, conversation);
          const response = await connection.request({ type: 'get_available_models' });
          return response.data;
        }
        case 'capability.save': {
          const value = record(payload.capability);
          const id = value.id === undefined ? shortId('cap') : identifier(value.id);
          const name = text(value.name, 'capability name');
          let capability: Capability;
          if (value.kind === 'mcp') capability = { id, name, kind: 'mcp', config: record(value.config) as Record<string, Json> };
          else if (value.kind === 'skill' || value.kind === 'extension') capability = { id, name, kind: value.kind, path: resolve(text(value.path, 'capability path')) };
          else throw new Error('Unknown capability kind');
          for (const runtime of [...runtimes.values()]) if (runtime.bot.capabilities.includes(id)) await stopRuntime(runtime.bot.id, runtime.conversation.id);
          return save(state => { const position = state.capabilities.findIndex(item => item.id === id); if (position < 0) state.capabilities.push(capability); else state.capabilities[position] = capability; return capability; });
        }
        case 'capability.delete': {
          const id = identifier(payload.id);
          for (const runtime of [...runtimes.values()]) if (runtime.bot.capabilities.includes(id)) await stopRuntime(runtime.bot.id, runtime.conversation.id);
          const affected = snapshot().bots.filter(bot => bot.capabilities.includes(id)).map(bot => bot.id);
          const result = await save(state => { state.capabilities = state.capabilities.filter(item => item.id !== id); for (const bot of state.bots) bot.capabilities = bot.capabilities.filter(item => item !== id); return { deleted: id }; });
          for (const botId of affected) await saveBotConfiguration(dataDir, botFor(botId));
          return result;
        }
        case 'config.list':
        case 'config.read':
        case 'config.write':
        case 'config.hindsightDefaults': {
          const bot = botFor(payload.botId);
          const result = await configurationCommand(dataDir, bot.id, action, payload);
          if (action === 'config.write') for (const runtime of runtimes.values()) if (runtime.bot.id === bot.id) runtime.dirty = true;
          return result;
        }
        case 'settings.save': {
          const value = record(payload.settings);
          if (value.nodeId !== undefined && value.nodeId !== snapshot().settings.nodeId) throw new Error('Node identity changes require a separate data directory');
          const result = await save(state => {
            for (const key of ['theme', 'customCss'] as const) if (typeof value[key] === 'string') state.settings[key] = value[key];
            if (value.locale === 'en' || value.locale === 'zh-CN') state.settings.locale = value.locale;
            for (const key of ['hostPlugins', 'rendererPlugins', 'peers'] as const) if (Array.isArray(value[key])) state.settings[key] = value[key].map(item => text(item, key));
            return state.settings;
          });
          return result;
        }
        case 'renderer.plugins': {
          const bot = botFor(payload.botId);
          const paths = (bot.plugins?.renderer ?? []).map((path, index) =>
            /^(https?:)?\/\//.test(path) ? path : `/bot-plugins/${encodeURIComponent(bot.id)}/${index}.js`);
          return { paths, sources: bot.plugins?.renderer ?? [] };
        }
        case 'widget.respond': {
          const id = text(payload.id, 'widget id');
          const view = views.get(id);
          const waiter = answers.get(id);
          if (view === undefined || view.state !== 'open') throw new Error('Widget is no longer open');
          const cancelled = payload.cancelled === true;
          const answer = payload.answer as Json | undefined;
          view.state = cancelled ? 'cancelled' : 'answered';
          if (answer !== undefined) view.answer = answer;
          emit({ type: 'widget', view: structuredClone(view) });
          answers.delete(id);
          waiter?.resolve({ id, cancelled, answer });
          return { answered: true };
        }
        case 'memory.save': {
          const body = text(payload.text, 'memory text');
          const replace = payload.replace === undefined ? undefined : text(payload.replace, 'memory replace');
          const hasBot = payload.botId !== undefined;
          const hasConversation = payload.conversationId !== undefined;
          if (hasBot === hasConversation) throw new Error('memory.save needs exactly one of botId or conversationId');
          const result = await save(state => {
            const target = hasBot ? state.bots.find(item => item.id === payload.botId) : state.conversations.find(item => item.id === payload.conversationId);
            if (target === undefined) throw new Error('Memory owner not found');
            if (replace === undefined) {
              if (target.memory.includes(body)) return { memory: target.memory };
              target.memory = target.memory.length === 0 ? `- ${body}` : `${target.memory}\n- ${body}`;
            } else {
              if (!target.memory.includes(replace)) throw new Error(`Memory text not found: ${replace}`);
              target.memory = target.memory.replace(replace, body);
            }
            return { memory: target.memory };
          });
          if (hasBot) await saveBotConfiguration(dataDir, botFor(payload.botId));
          return result;
        }
        case 'onlyne.status': return (await bridgeFor(payload.botId === undefined ? defaultBot() : botFor(payload.botId))).status();
        case 'onlyne.action': return (await bridgeFor(payload.botId === undefined ? defaultBot() : botFor(payload.botId))).action(text(payload.action, 'Onlyne action'), payload.args === undefined ? {} : record(payload.args));
        default: throw new Error(`Unknown desktop action: ${action}`);
      }
    },
    async close() {
      if (closed) return;
      closed = true;
      for (const waiter of answers.values()) waiter.reject(new Error('Desktop app closed'));
      answers.clear();
      await Promise.all([...runtimes.values()].map(async runtime => { for (const call of runtime.calls.values()) call.abort(); await (await runtime.connection).close(); }));
      await Promise.all([...routers.values()].map(router => router.close()));
      for (const scoped of botExtensions.values()) for (const plugin of [...scoped.plugins].reverse()) await plugin.dispose?.();
      for (const plugin of [...plugins].reverse()) await plugin.dispose?.();
      await Promise.all([...bridges.values()].map(async bridge => (await bridge).close()));
      await store.close();
      listeners.clear();
    },
  };

  for (const tool of createTeamTools(controller)) api.registerTool(tool);
  api.registerTool({
    name: 'msg', label: 'Onlyne message', description: 'Communicate or inspect the Onlyne network. Express delivery intent in delivery_kind and preserve explicit references in meta.',
    parameters: { type: 'object', properties: { delivery_kind: { type: 'string' }, msg: { type: 'string' }, meta: { type: 'object', additionalProperties: true } }, required: ['delivery_kind', 'msg'] },
    async execute(args, context) { return toolResult(await (await bridgeFor(botFor(context.bot.id))).dispatch({ delivery_kind: text(args.delivery_kind, 'delivery_kind'), msg: typeof args.msg === 'string' ? args.msg : '', ...(args.meta === undefined ? {} : { meta: record(args.meta) }) }, context)); },
  });
  api.registerTool({
    name: 'show_media', label: 'Show media',
    description: 'Show an image, video, audio clip, SVG, generated picture or sticker in this conversation. Use a local file path (absolute, or relative to your workspace) or inline SVG text. The file is copied into the app media store and displayed with the caption.',
    parameters: {
      type: 'object',
      properties: {
        path: { type: 'string', description: 'Local file path, absolute or relative to the workspace.' },
        url: { type: 'string', description: 'An http(s) URL to display without copying.' },
        svg: { type: 'string', description: 'Inline SVG markup.' },
        sticker: { type: 'string', description: 'Sticker name under .omp/stickers.' },
        caption: { type: 'string', description: 'Text shown with the media.' },
        contentType: { type: 'string', description: 'MIME type when the file extension is unusual.' },
      },
    },
    async execute(args, context) {
      let item: MediaItem;
      if (typeof args.sticker === 'string') {
        item = await media.fromPath(stickerPath(dataDir, context.bot.id, args.sticker), { botId: context.bot.id, name: args.sticker, kind: 'sticker' });
      } else if (typeof args.svg === 'string') {
        item = await media.put(new TextEncoder().encode(args.svg), { name: 'drawing.svg', contentType: 'image/svg+xml', botId: context.bot.id });
      } else if (typeof args.path === 'string') {
        const resolved = await resolveMediaPath(dataDir, context.bot.id, context.conversation.id, context.bot.workspace, args.path);
        item = await media.fromPath(resolved, { botId: context.bot.id });
      } else if (typeof args.url === 'string') {
        if (!/^https?:\/\//.test(args.url)) throw new Error('A media URL must be http or https');
        const contentType = typeof args.contentType === 'string' ? args.contentType : '';
        item = { id: shortId('m'), kind: mediaKind(contentType, args.url).kind, contentType: contentType.length > 0 ? contentType : 'image/*', name: args.url, size: 0, createdAt: Date.now(), botId: context.bot.id, url: args.url };
        await save(state => { state.media.push(item); });
      } else throw new Error('show_media needs path, url, svg or sticker');
      const caption = typeof args.caption === 'string' ? args.caption : '';
      const message: ChatMessage = { id: shortId('msg'), conversationId: context.conversation.id, role: 'assistant', botId: context.bot.id, text: caption, media: [item], createdAt: Date.now() };
      await save(state => { state.messages.push(message); });
      return toolResult({ media: item, message: message.id, url: item.url ?? `/api/media/${item.id}` });
    },
  });
  api.registerTool({
    name: 'show_view', label: 'Interactive view', description: 'Display a rich widget in this conversation. Protocol onlyne.view/v1 supports text, markdown, metrics, table, chart, progress, choices, confirm, form, html and stack. Await a user response with await_reply.',
    parameters: { type: 'object', properties: { id: { type: 'string' }, protocol: { type: 'string' }, title: { type: 'string' }, view: {}, await_reply: { type: 'boolean' } }, required: ['view'] },
    async execute(args, context) {
      const id = typeof args.id === 'string' && args.id.length > 0 ? `${context.bot.id}/${context.conversation.id}/${args.id}` : shortId('v');
      if (answers.has(id)) throw new Error('This widget is already waiting for a response');
      const view: WidgetEnvelope = { id, protocol: typeof args.protocol === 'string' ? args.protocol : 'onlyne.view/v1', content: args.view as Json, interactive: args.await_reply === true, conversationId: context.conversation.id, botId: context.bot.id, requestId: id, state: 'open', ...(typeof args.title === 'string' ? { title: args.title } : {}) };
      views.set(id, view);
      if (args.await_reply !== true) { emit({ type: 'widget', view }); return toolResult({ id, displayed: true }); }
      const { promise, resolve: answer, reject } = Promise.withResolvers<unknown>();
      answers.set(id, { resolve: answer, reject });
      const cancel = (): void => { answers.delete(id); view.state = 'cancelled'; emit({ type: 'widget', view }); reject(new Error('Widget request cancelled')); };
      context.signal.addEventListener('abort', cancel, { once: true });
      emit({ type: 'widget', view });
      try { return toolResult(await promise); }
      finally { context.signal.removeEventListener('abort', cancel); }
    },
  });
  api.registerUri({ scheme: 'onlyne', description: 'Onlyne network status, messages, threads and resources', read: async (uri, context) => (await bridgeFor(botFor(context.bot.id))).read(uri) });
  api.registerUri({
    scheme: 'desktop', description: 'Bot team and shared conversation state',
    async read(uri, context) {
      if (uri === 'desktop://team') return { content: JSON.stringify(snapshot().bots.map(({ id, name, description }) => ({ id, name, description, team_admin: id === snapshot().teamAdminId }))), contentType: 'application/json' };
      if (uri === 'desktop://memory') return { content: context.bot.memory, contentType: 'text/plain' };
      if (uri === 'desktop://conversation') return { content: JSON.stringify(snapshot().messages.filter(item => item.conversationId === context.conversation.id)), contentType: 'application/json' };
      if (uri === 'desktop://scene') {
        const conversation = snapshot().conversations.find(item => item.id === context.conversation.id) ?? context.conversation;
        return {
          content: JSON.stringify({
            conversation: { id: conversation.id, title: conversation.title, kind: conversation.kind, adminBotId: conversation.adminBotId },
            scene: conversation.scene ?? null, persona: { name: conversation.personaName, description: conversation.personaDescription },
            authorNote: conversation.authorNote ?? null, greeting: conversation.greeting ?? null, greeted: conversation.greeted === true,
            members: conversation.botIds.map(id => { const bot = snapshot().bots.find(item => item.id === id); return { id, name: bot?.name ?? id, avatar: bot?.character?.avatar }; }),
            messageCount: snapshot().messages.filter(item => item.conversationId === conversation.id).length,
          }, null, 2),
          contentType: 'application/json',
        };
      }
      const publicBot = (id: string) => {
        const bot = snapshot().bots.find(item => item.id === id);
        if (bot === undefined) throw new Error(`Bot not found: ${id}`);
        return { id: bot.id, name: bot.name, description: bot.description, team_admin: bot.id === snapshot().teamAdminId, model: bot.model ?? null, thinking: bot.thinking, character: bot.character ?? null, lorebook: bot.lorebook ?? null, nodeId: bot.onlyne?.nodeId ?? null };
      };
      if (uri.startsWith('desktop://bots/')) return { content: JSON.stringify(publicBot(decodeURIComponent(uri.slice('desktop://bots/'.length))), null, 2), contentType: 'application/json' };
      if (uri === 'desktop://bots') {
        return { content: JSON.stringify(snapshot().bots.map(bot => publicBot(bot.id)), null, 2), contentType: 'application/json' };
      }
      if (uri === 'desktop://health') return { content: JSON.stringify(healthSummary(), null, 2), contentType: 'application/json' };
      if (uri.startsWith('desktop://conversation/')) {
        const id = decodeURIComponent(uri.slice('desktop://conversation/'.length));
        const conversation = snapshot().conversations.find(item => item.id === id);
        if (conversation === undefined) throw new Error(`Conversation not found: ${id}`);
        return { content: JSON.stringify({ conversation, messages: snapshot().messages.filter(item => item.conversationId === id).slice(-50) }, null, 2), contentType: 'application/json' };
      }
      throw new Error(`Unknown desktop URI: ${uri}`);
    },
  });

  try {
    const configured = snapshot().settings;
    const configuredBots = await Promise.all(snapshot().bots.map(bot => loadBotConfiguration(dataDir, bot)));
    await store.update(state => { state.bots = configuredBots; });
    for (const bot of configuredBots) await bridgeFor(bot);
    for (const supplied of options.plugins ?? []) { await supplied.setup(api); plugins.push(supplied); }
    for (const path of configured.hostPlugins) {
      const module = await import(pathToFileURL(resolve(path)).href);
      const plugin = module.default as DesktopPlugin;
      if (plugin === undefined || typeof plugin.setup !== 'function') throw new Error(`Host plugin has no default DesktopPlugin export: ${path}`);
      await plugin.setup(api);
      plugins.push(plugin);
    }
    return controller;
  } catch (error) {
    for (const plugin of [...plugins].reverse()) await plugin.dispose?.();
    await Promise.all([...bridges.values()].map(async bridge => (await bridge).close()));
    await store.close();
    throw error;
  }
}
