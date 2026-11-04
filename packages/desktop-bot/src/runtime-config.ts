import { randomUUID } from 'node:crypto';
import { lstat, mkdir, readdir, readFile, readlink, realpath, rename, rmdir, stat, symlink, unlink, writeFile } from 'node:fs/promises';
import type { Dirent } from 'node:fs';
import { homedir } from 'node:os';
import path from 'node:path';
import type { Capability, RuntimeContext } from './contracts.ts';

const MCP_SCHEMA = 'https://raw.githubusercontent.com/can1357/oh-my-pi/main/packages/coding-agent/src/config/mcp-schema.json';
const MANIFEST = 'desktop-runtime.json';
const THINKING: Record<string, true> = { off: true, minimal: true, low: true, medium: true, high: true, xhigh: true, max: true };

export interface PreparedRuntime {
  cwd: string;
  args: string[];
  env: Record<string, string>;
}

interface RuntimeManifest {
  version: 1;
  mcp: { created: boolean; servers: Record<string, { config: unknown }> };
  skills: { rootCreated: boolean; links: Record<string, { target: string }> };
}

export function botConfigRoot(dataDir: string, botId: string): string {
  return path.join(path.resolve(dataDir), 'bots', botId);
}

export function runtimeWorkspace(dataDir: string, botId: string, conversationId: string): string {
  return path.join(path.resolve(dataDir), 'runtimes', botId, conversationId, 'workspace');
}

export async function prepareRuntime(context: RuntimeContext): Promise<PreparedRuntime> {
  const { bot, conversation } = context;
  if (!/^[A-Za-z0-9_-]+$/.test(bot.id)) throw new Error(`Bot id cannot be used as a directory name: ${bot.id}`);
  if (!/^[A-Za-z0-9_-]+$/.test(conversation.id)) throw new Error(`Conversation id cannot be used as a directory name: ${conversation.id}`);
  const configRoot = botConfigRoot(context.dataDir, bot.id);
  const configDir = path.join(configRoot, '.omp');
  const cwd = runtimeWorkspace(context.dataDir, bot.id, conversation.id);
  const sessionDir = path.resolve(context.sessionDir);
  const workspace = typeof bot.workspace === 'string' ? bot.workspace.trim() : '';
  if (!workspace) throw new Error(`Bot ${bot.id} has no workspace`);
  const workspaceDir = path.isAbsolute(workspace) ? workspace : path.resolve(workspace);

  await mkdir(cwd, { recursive: true });
  await mkdir(configDir, { recursive: true });
  await mkdir(sessionDir, { recursive: true });
  await linkProjectConfig(cwd, configDir);
  await mergePluginOverrides(configDir);

  const manifest = await loadManifest(configDir);
  await applyMcp(configDir, context.capabilities, manifest);
  const skillNames = await applySkills(configDir, context.capabilities, manifest);

  const extensionPaths = await botExtensions(configDir);
  const explicitExtensions: string[] = [];
  for (const capability of context.capabilities) {
    if (capability.kind !== 'extension') continue;
    const entry = path.resolve(capability.path);
    if (await isOnlyneExtension(entry)) continue;
    const info = await stat(entry).catch(() => undefined);
    if (info === undefined) throw new Error(`Extension capability ${capability.id} path does not exist: ${entry}`);
    extensionPaths.push(entry);
    explicitExtensions.push(entry);
  }
  for (const name of await extensionSkillNames(cwd, extensionPaths)) skillNames.add(name);

  // Native discovery stays on so a bot inherits OMP's plugin ecology from its own .omp; the
  // per-bot plugin-overrides file removes the ambient omp-onlyne extension, whose node would
  // compete with the host-owned one.
  const args: string[] = ['--session-dir', sessionDir, '--continue', '--cwd', cwd, '--add-dir', workspaceDir];

  if (bot.model !== undefined) {
    const provider = bot.model.provider.trim();
    const id = bot.model.id.trim();
    if (!provider || !id || /\s/.test(provider) || /\s/.test(id)) throw new Error(`Bot ${bot.id} has an invalid model ${JSON.stringify(bot.model)}`);
    args.push('--model', `${provider}/${id}`);
  }
  if (bot.thinking !== 'inherit') {
    if (THINKING[bot.thinking] !== true) throw new Error(`Bot ${bot.id} has an invalid thinking level: ${String(bot.thinking)}`);
    args.push('--thinking', bot.thinking);
  }

  // Bot-local roots load through native project discovery; only selected capabilities are explicit.
  for (const entry of [...new Set(explicitExtensions)]) args.push('-e', entry);

  if (skillNames.size === 0) args.push('--no-skills');
  else args.push('--skills', [...skillNames].map(name => name.replace(/([*?[\]{}!])/g, '[$1]')).join(','));

  const runtimeDir = path.join(sessionDir, 'runtime');
  if (context.systemPrompt !== undefined) {
    const file = path.join(runtimeDir, 'system-prompt.md');
    await writeAtomicIfChanged(file, context.systemPrompt);
    args.push('--system-prompt', file);
  }
  const appendFile = path.join(runtimeDir, 'append-system-prompt.md');
  await writeAtomicIfChanged(appendFile, await buildAppendPrompt(context, cwd, workspaceDir, configDir));
  args.push('--append-system-prompt', appendFile);

  if (context.toolNames !== undefined) {
    const names = context.toolNames.map(name => name.trim()).filter(Boolean);
    if (names.length === 0) args.push('--no-tools');
    else args.push('--tools', names.join(','));
  }

  await saveManifest(configDir, manifest);
  return { cwd, args, env: {} };
}

async function buildAppendPrompt(context: RuntimeContext, cwd: string, workspace: string, configDir: string): Promise<string> {
  const { bot, conversation } = context;
  const sections: string[] = [
    `You are ${bot.name} (${bot.id}), one bot in a team running inside the Onlyne desktop host.`,
  ];
  if (bot.description.trim()) sections.push(bot.description.trim());
  if (bot.character !== undefined) {
    const card = bot.character;
    const flavor = [
      card.description.trim(),
      card.personality === undefined ? '' : `Personality: ${card.personality.trim()}`,
      card.scenario === undefined ? '' : `Scenario: ${card.scenario.trim()}`,
      card.systemPrompt === undefined ? '' : card.systemPrompt.trim(),
    ].filter(part => part.length > 0);
    if (flavor.length > 0) sections.push('## Character', flavor.join('\n\n'));
  }
  sections.push('## Instructions', bot.instructions.trim() || 'No additional instructions were provided.', '## Bot memory', bot.memory.trim() || 'No bot memory was recorded.');
  sections.push(`## Conversation memory — ${conversation.title}`, conversation.memory.trim() || 'No conversation memory was recorded.');
  sections.push('## Workspaces', `Runtime workspace: ${cwd}. This is your working directory; its .omp points at the bot configuration root ${configDir}.`, `User workspace: ${workspace}. It is added with --add-dir; put deliverables the user asked for there.`);
  sections.push('## Desktop host tools', '- desktop_team — coordinate with the other bots in this desktop team, inspect the shared conversation, and configure automatic group turns.', '- msg — send and read Onlyne network messages. Pass a delivery_kind and preserve explicit thread and reply references in meta.', '- show_view — render a rich interactive view in this conversation (onlyne.view/v1: text, markdown, metrics, table, chart, progress, choices, confirm, form, html, stack). Set await_reply to wait for the user answer.', 'Read desktop://team, desktop://scene, desktop://bots, desktop://health, desktop://memory and desktop://conversation for team roster, current scene, character cards, health, memory and transcript state.', 'Onlyne messaging is provided by the desktop host. Do not install or load an omp-onlyne extension; use msg and the other host tools above.');
  const appended = await readText(path.join(configDir, 'APPEND_SYSTEM.md'));
  if (appended !== undefined && appended.trim()) sections.push('## Bot APPEND_SYSTEM.md', appended.trim());
  const userAppended = await readText(path.join(homedir(), '.omp', 'agent', 'APPEND_SYSTEM.md'));
  if (userAppended !== undefined && userAppended.trim()) sections.push('## User APPEND_SYSTEM.md', userAppended.trim());
  return `${sections.join('\n\n')}\n`;
}

async function applyMcp(configDir: string, capabilities: Capability[], manifest: RuntimeManifest): Promise<void> {
  const desired = new Map<string, Record<string, unknown>>();
  for (const capability of capabilities) {
    if (capability.kind !== 'mcp') continue;
    if (desired.has(capability.id)) throw new Error(`MCP capability id is duplicated: ${capability.id}`);
    desired.set(capability.id, normalizeMcp(capability));
  }

  const file = path.join(configDir, 'mcp.json');
  const current = await readJsonObject(file);
  const base: Record<string, unknown> = current === undefined ? { $schema: MCP_SCHEMA, mcpServers: {} } : structuredClone(current);
  const servers = base.mcpServers;
  let table: Record<string, unknown>;
  if (servers === undefined) {
    table = {};
    base.mcpServers = table;
  } else {
    if (!isRecord(servers)) throw new Error(`${file} has a non-object mcpServers value`);
    table = servers;
  }

  const owned = manifest.mcp.servers;
  const hadOwnership = Object.keys(owned).length > 0;
  const nextOwned: Record<string, { config: unknown }> = {};
  let changed = current === undefined && desired.size > 0;

  for (const [name, config] of desired) {
    const existing = table[name];
    const prior = owned[name];
    if (existing === undefined) {
      table[name] = config;
      nextOwned[name] = { config };
      changed = true;
    } else if (prior !== undefined) {
      if (!jsonEqual(existing, prior.config)) {
        throw new Error(`MCP server "${name}" in ${file} was edited outside the desktop app; remove or rename it before selecting capability ${name}`);
      }
      if (!jsonEqual(existing, config)) {
        table[name] = config;
        changed = true;
      }
      nextOwned[name] = { config };
    } else if (!jsonEqual(existing, config)) {
      throw new Error(`MCP server name collision: ${file} already defines "${name}" with a different configuration`);
    }
  }

  for (const name of Object.keys(owned)) {
    if (desired.has(name)) continue;
    const existing = table[name];
    if (existing === undefined) continue;
    const prior = owned[name];
    if (prior !== undefined && jsonEqual(existing, prior.config)) {
      delete table[name];
      changed = true;
    }
  }
  manifest.mcp.servers = nextOwned;

  if (current === undefined && !changed) {
    if (manifest.mcp.created || hadOwnership) {
      manifest.mcp.created = false;
      manifest.mcp.servers = {};
      await saveManifest(configDir, manifest);
    }
    return;
  }
  const empty = Object.keys(table).length === 0;
  if (manifest.mcp.created && empty && isOurShell(base)) {
    await unlink(file).catch(error => { if (!isMissing(error)) throw error; });
    manifest.mcp.created = false;
    manifest.mcp.servers = {};
    await saveManifest(configDir, manifest);
    return;
  }
  if (changed) {
    await writeAtomicIfChanged(file, `${JSON.stringify(base, null, 2)}\n`);
    if (current === undefined) manifest.mcp.created = true;
  }
  await saveManifest(configDir, manifest);
}

function isOurShell(value: Record<string, unknown>): boolean {
  for (const key of Object.keys(value)) if (key !== '$schema' && key !== 'mcpServers') return false;
  return value.$schema === MCP_SCHEMA;
}

function normalizeMcp(capability: Capability): Record<string, unknown> {
  if (capability.kind !== 'mcp') throw new Error(`Capability ${capability.id} is not an MCP server`);
  if (!/^[A-Za-z0-9_.:-][A-Za-z0-9_.: -]{0,99}$/.test(capability.id)) throw new Error(`MCP capability id is not a valid server name: ${capability.id}`);
  if (!isRecord(capability.config)) throw new Error(`MCP capability ${capability.id} has no object config`);
  const config: Record<string, unknown> = { ...capability.config };
  const type = config.type ?? (typeof config.command === 'string' ? 'stdio' : typeof config.url === 'string' ? 'http' : undefined);
  if (type !== 'stdio' && type !== 'http' && type !== 'sse') throw new Error(`MCP capability ${capability.id} has an unsupported transport type: ${String(config.type)}`);
  if (config.command !== undefined && config.url !== undefined) throw new Error(`MCP capability ${capability.id} sets both command and url`);
  if (type === 'stdio') {
    if (typeof config.command !== 'string' || !config.command.trim()) throw new Error(`MCP capability ${capability.id} requires a command`);
    delete config.url;
  } else {
    if (typeof config.url !== 'string' || !config.url.trim()) throw new Error(`MCP capability ${capability.id} requires a url`);
    delete config.command;
  }
  if (config.args !== undefined && (!Array.isArray(config.args) || config.args.some(value => typeof value !== 'string'))) throw new Error(`MCP capability ${capability.id} has invalid args`);
  for (const key of ['env', 'headers'] as const) {
    const value = config[key];
    if (value === undefined) continue;
    if (!isRecord(value) || Object.values(value).some(entry => typeof entry !== 'string')) throw new Error(`MCP capability ${capability.id} has invalid ${key}`);
  }
  if (config.cwd !== undefined && typeof config.cwd !== 'string') throw new Error(`MCP capability ${capability.id} has an invalid cwd`);
  if (config.timeout !== undefined && (typeof config.timeout !== 'number' || !Number.isFinite(config.timeout) || config.timeout < 0)) throw new Error(`MCP capability ${capability.id} has an invalid timeout`);
  for (const key of ['enabled', 'instructions'] as const) {
    if (config[key] !== undefined && typeof config[key] !== 'boolean') throw new Error(`MCP capability ${capability.id} has an invalid ${key}`);
  }
  if (config.requestIdFormat !== undefined && config.requestIdFormat !== 'number' && config.requestIdFormat !== 'string') throw new Error(`MCP capability ${capability.id} has an invalid requestIdFormat`);
  config.type = type;
  return config;
}

async function applySkills(configDir: string, capabilities: Capability[], manifest: RuntimeManifest): Promise<Set<string>> {
  const desired = new Map<string, { target: string; name: string }>();
  for (const capability of capabilities) {
    if (capability.kind !== 'skill') continue;
    const resolved = await resolveSkill(capability);
    desired.set(capability.id, resolved);
  }

  const skillsRoot = path.join(configDir, 'skills');
  const nextLinks: Record<string, { target: string }> = {};
  let rootCreated = manifest.skills.rootCreated;
  if (desired.size > 0 && !rootCreated) {
    const info = await stat(skillsRoot).catch(() => undefined);
    if (info === undefined) {
      await mkdir(skillsRoot, { recursive: true });
      rootCreated = true;
    } else if (!info.isDirectory()) {
      throw new Error(`${skillsRoot} is not a directory`);
    }
  }

  for (const [id, skill] of desired) {
    if (!/^[A-Za-z0-9_-]+$/.test(id)) throw new Error(`Skill capability id cannot be used as a directory name: ${id}`);
    const link = path.join(skillsRoot, id);
    const prior = manifest.skills.links[id];
    const info = await lstat(link).catch(error => { if (isMissing(error)) return undefined; throw error; });
    if (info === undefined) {
      try {
        await symlink(skill.target, link, 'dir');
      } catch (error) {
        const current = await lstat(link).catch(() => undefined);
        if (current === undefined || !current.isSymbolicLink() || path.resolve(path.dirname(link), await readlink(link)) !== path.resolve(skill.target)) throw error;
      }
      nextLinks[id] = { target: skill.target };
    } else if (info.isSymbolicLink()) {
      const current = await readlink(link);
      if (path.resolve(path.dirname(link), current) !== path.resolve(skill.target)) {
        if (prior === undefined) throw new Error(`Skill link collision: ${link} already points at ${current}`);
        await unlink(link);
        await symlink(skill.target, link, 'dir');
      }
      nextLinks[id] = { target: skill.target };
    } else {
      throw new Error(`Skill link collision: ${link} already exists and is not managed by the desktop app`);
    }
  }

  for (const [id, prior] of Object.entries(manifest.skills.links)) {
    if (desired.has(id)) continue;
    const link = path.join(skillsRoot, id);
    const info = await lstat(link).catch(error => { if (isMissing(error)) return undefined; throw error; });
    if (info === undefined || !info.isSymbolicLink()) continue;
    if (path.resolve(path.dirname(link), await readlink(link)) !== path.resolve(prior.target)) continue;
    await unlink(link);
  }
  manifest.skills.links = nextLinks;
  manifest.skills.rootCreated = rootCreated;

  const names = new Set<string>();
  for (const skill of desired.values()) names.add(skill.name);
  for (const entry of await readSkillDirectories(skillsRoot)) {
    if (desired.has(entry)) continue;
    names.add((await readSkillName(path.join(skillsRoot, entry))) ?? entry);
  }

  if (rootCreated) {
    await rmdir(skillsRoot).catch(error => {
      const code = error instanceof Error && 'code' in error ? error.code : undefined;
      if (code !== 'ENOTEMPTY' && code !== 'ENOENT') throw error;
    });
    const still = await stat(skillsRoot).catch(() => undefined);
    if (still === undefined) manifest.skills.rootCreated = false;
  }
  await saveManifest(configDir, manifest);
  return names;
}

async function resolveSkill(capability: Capability): Promise<{ target: string; name: string }> {
  if (capability.kind !== 'skill') throw new Error(`Capability ${capability.id} is not a skill`);
  const source = path.resolve(capability.path);
  const info = await stat(source).catch(() => undefined);
  if (info === undefined) throw new Error(`Skill capability ${capability.id} path does not exist: ${source}`);
  let dir: string;
  if (info.isFile()) {
    if (path.basename(source) !== 'SKILL.md') throw new Error(`Skill capability ${capability.id} must point at a skill directory or SKILL.md, not ${source}`);
    dir = path.dirname(source);
  } else if (info.isDirectory()) {
    dir = source;
    const skillFile = await stat(path.join(dir, 'SKILL.md')).catch(() => undefined);
    if (skillFile === undefined || !skillFile.isFile()) throw new Error(`Skill capability ${capability.id} directory has no SKILL.md: ${dir}`);
  } else {
    throw new Error(`Skill capability ${capability.id} path is neither a file nor a directory: ${source}`);
  }
  return { target: dir, name: (await readSkillName(dir)) ?? capability.id };
}

async function readSkillDirectories(skillsRoot: string): Promise<string[]> {
  const entries = await readdir(skillsRoot, { withFileTypes: true }).catch((error): Dirent[] => { if (isMissing(error)) return []; throw error; });
  const result: string[] = [];
  for (const entry of entries) {
    if (entry.name.startsWith('.')) continue;
    const full = path.join(skillsRoot, entry.name);
    const info = await stat(full).catch(() => undefined);
    if (info === undefined || !info.isDirectory()) continue;
    result.push(entry.name);
  }
  return result;
}

async function readSkillName(dir: string): Promise<string | undefined> {
  const text = await readText(path.join(dir, 'SKILL.md'));
  if (text === undefined) return undefined;
  if (!text.startsWith('---')) return undefined;
  const end = text.indexOf('\n---', 3);
  if (end < 0) return undefined;
  const parsed = parseYaml(text.slice(3, end));
  if (!isRecord(parsed) || typeof parsed.name !== 'string') return undefined;
  const name = parsed.name.trim();
  if (!name || name.includes('/') || name.includes('\\') || name.includes(',')) return undefined;
  return name;
}

async function mergePluginOverrides(configDir: string): Promise<void> {
  const file = path.join(configDir, 'plugin-overrides.json');
  let text = await readText(file);
  let source = file;
  if (text === undefined) {
    for (const base of ['.claude', '.codex', '.gemini']) {
      const fallback = path.join(path.dirname(configDir), base, 'plugin-overrides.json');
      text = await readText(fallback);
      if (text !== undefined) {
        source = fallback;
        break;
      }
    }
  }
  let value: Record<string, unknown> = {};
  if (text !== undefined) {
    let parsed: unknown;
    try {
      parsed = parseJson(text);
    } catch {
      throw new Error(`${source} is not valid JSON; refusing to rewrite it`);
    }
    if (!isRecord(parsed)) throw new Error(`${source} is not a JSON object`);
    value = structuredClone(parsed);
  }
  const disabled = value.disabled;
  if (disabled !== undefined && !Array.isArray(disabled)) throw new Error(`${source} has a non-array disabled field`);
  const names = Array.isArray(disabled) ? disabled : [];
  if (names.includes('omp-onlyne')) return;
  names.push('omp-onlyne');
  value.disabled = names;
  await writeAtomicIfChanged(file, `${JSON.stringify(value, null, 2)}\n`);
}

async function readPluginOverrides(configDir: string): Promise<Set<string>> {
  const blocked = new Set<string>();
  const bases = [configDir, ...['.claude', '.codex', '.gemini'].map(base => path.join(path.dirname(configDir), base))];
  for (const base of bases) {
    const file = path.join(base, 'plugin-overrides.json');
    const overrides = await readLooseJson(file);
    if (overrides === undefined) continue;
    if (isRecord(overrides) && Array.isArray(overrides.disabled)) {
      for (const name of overrides.disabled) if (typeof name === 'string') blocked.add(name);
    }
    break;
  }
  return blocked;
}

async function linkProjectConfig(cwd: string, configDir: string): Promise<void> {
  const link = path.join(cwd, '.omp');
  const info = await lstat(link).catch(error => { if (isMissing(error)) return undefined; throw error; });
  if (info === undefined) {
    await symlink(configDir, link, 'dir');
    return;
  }
  if (!info.isSymbolicLink()) throw new Error(`${link} exists and is not managed by the desktop app`);
  const target = path.resolve(path.dirname(link), await readlink(link));
  if (target !== path.resolve(configDir)) throw new Error(`${link} points at ${target}, expected ${configDir}`);
}

async function botExtensions(configDir: string): Promise<string[]> {
  const settings = readSettingsLists(await readLooseJson(path.join(configDir, 'settings.json')));
  const config = readSettingsLists((await readLooseYaml(path.join(configDir, 'config.yml'))) ?? (await readLooseYaml(path.join(configDir, 'config.yaml'))));
  const disabled = new Set(config.disabled ?? settings.disabled ?? []);
  const listed = config.extensions ?? settings.extensions ?? [];

  const result: string[] = [];
  await collectExtensionDir(path.join(configDir, 'extensions'), disabled, result);
  for (const packageDir of await enabledPluginPackages(path.join(configDir, 'plugins'), await readPluginOverrides(configDir))) {
    const name = path.basename(packageDir);
    if (!disabled.has(`extension-module:${name}`)) result.push(packageDir);
  }
  for (const phase of ['pre', 'post']) await collectHookDir(path.join(configDir, 'hooks', phase), disabled, result);
  for (const entry of listed) if (!isDisabled(entry, disabled)) result.push(expandPath(entry, path.dirname(configDir)));
  const kept: string[] = [];
  for (const entry of result) if (!(await isOnlyneExtension(entry))) kept.push(entry);
  return kept;
}

function readSettingsLists(value: unknown): { extensions?: string[]; disabled?: string[] } {
  if (!isRecord(value)) return {};
  const strings = (field: unknown): string[] | undefined => Array.isArray(field)
    ? field.filter((item): item is string => typeof item === 'string' && item.trim().length > 0).map(item => item.trim())
    : undefined;
  return { extensions: strings(value.extensions), disabled: strings(value.disabledExtensions) };
}

async function collectExtensionDir(dir: string, disabled: Set<string>, result: string[]): Promise<void> {
  const entries = await readdir(dir, { withFileTypes: true }).catch((error): Dirent[] => { if (isMissing(error)) return []; throw error; });
  for (const entry of entries.sort((left, right) => left.name.localeCompare(right.name))) {
    if (entry.name.startsWith('.')) continue;
    const full = path.join(dir, entry.name);
    const info = await stat(full).catch(() => undefined);
    if (info === undefined) continue;
    if (info.isFile()) {
      if (/\.(ts|js)$/.test(entry.name) && !isDisabled(full, disabled)) result.push(full);
      continue;
    }
    if (!info.isDirectory() || isDisabled(full, disabled)) continue;
    if ((await declaredExtensions(full)) !== undefined || (await resolveIndex(full)) !== undefined) result.push(full);
  }
}

async function collectHookDir(dir: string, disabled: Set<string>, result: string[]): Promise<void> {
  const entries = await readdir(dir, { withFileTypes: true }).catch((error): Dirent[] => { if (isMissing(error)) return []; throw error; });
  for (const entry of entries.sort((left, right) => left.name.localeCompare(right.name))) {
    if (entry.name.startsWith('.') || !/\.(ts|js)$/.test(entry.name)) continue;
    const full = path.join(dir, entry.name);
    if (!isDisabled(full, disabled)) result.push(full);
  }
}

async function resolveIndex(dir: string): Promise<string | undefined> {
  for (const suffix of ['ts', 'js']) {
    const candidate = path.join(dir, `index.${suffix}`);
    const info = await stat(candidate).catch(() => undefined);
    if (info?.isFile()) return candidate;
  }
  return undefined;
}

function isDisabled(entry: string, disabled: Set<string>): boolean {
  return disabled.has(`extension-module:${extensionName(entry)}`);
}

function extensionName(entry: string): string {
  const base = path.basename(entry);
  if (/^index\.(ts|js|mjs|cjs)$/.test(base)) return path.basename(path.dirname(entry));
  return base.replace(/\.(ts|js|mjs|cjs)$/, '');
}

async function enabledPluginPackages(root: string, blocked: Set<string>): Promise<string[]> {
  const pkg = await readLooseJson(path.join(root, 'package.json'));
  const lock = await readLooseJson(path.join(root, 'omp-plugins.lock.json'));
  const declared = new Set<string>();
  if (isRecord(pkg) && isRecord(pkg.dependencies)) for (const name of Object.keys(pkg.dependencies)) declared.add(name);
  const locked = new Map<string, unknown>();
  if (isRecord(lock) && isRecord(lock.plugins)) for (const [name, entry] of Object.entries(lock.plugins)) locked.set(name, entry);
  const result: string[] = [];
  for (const name of new Set([...declared, ...locked.keys()])) {
    if (name === 'omp-onlyne' || blocked.has(name)) continue;
    const entry = locked.get(name);
    if (isRecord(entry) && entry.enabled === false) continue;
    const packageDir = path.join(root, 'node_modules', ...name.split('/'));
    if (pkg !== undefined && !declared.has(name)) {
      const link = await lstat(packageDir).catch(() => undefined);
      if (link === undefined || !link.isSymbolicLink()) continue;
    }
    if ((await declaredExtensions(packageDir)) === undefined) continue;
    result.push(packageDir);
  }
  return result;
}

async function declaredExtensions(dir: string): Promise<string[] | undefined> {
  const pkg = await readLooseJson(path.join(dir, 'package.json'));
  if (!isRecord(pkg)) return undefined;
  for (const key of ['omp', 'pi'] as const) {
    const scope = pkg[key];
    if (!isRecord(scope) || !Array.isArray(scope.extensions)) continue;
    const declared = scope.extensions.filter((entry): entry is string => typeof entry === 'string' && entry.trim().length > 0);
    if (declared.length > 0) return declared;
  }
  return undefined;
}

async function extensionSkillNames(root: string, entries: string[]): Promise<string[]> {
  const names = new Set<string>();
  for (const entry of entries) {
    const resolved = expandPath(entry, root);
    const info = await stat(resolved).catch(() => undefined);
    if (info === undefined) continue;
    const skills = info.isDirectory() ? path.join(resolved, 'skills') : path.join(path.dirname(resolved), 'skills');
    for (const name of await readSkillDirectories(skills)) {
      const skill = await readSkillName(path.join(skills, name));
      names.add(skill ?? name);
    }
  }
  return [...names];
}

function expandPath(value: string, cwd: string): string {
  const expanded = value.replace(/^~(?=$|[\\/])/, homedir());
  return path.isAbsolute(expanded) ? expanded : path.resolve(cwd, expanded);
}

async function isOnlyneExtension(entry: string): Promise<boolean> {
  const candidates = [entry];
  const resolved = await realpath(entry).catch(() => undefined);
  if (resolved !== undefined && resolved !== entry) candidates.push(resolved);
  for (const candidate of candidates) {
    if (candidate.split(/[\\/]/).includes('omp-onlyne')) return true;
  }
  return false;
}

async function loadManifest(configDir: string): Promise<RuntimeManifest> {
  const file = path.join(configDir, MANIFEST);
  const text = await readText(file);
  if (text === undefined) return { version: 1, mcp: { created: false, servers: {} }, skills: { rootCreated: false, links: {} } };
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    throw new Error(`${file} is not valid JSON`);
  }
  if (!isRecord(value)) throw new Error(`${file} is not a JSON object`);
  const mcp = value.mcp;
  const skills = value.skills;
  if (value.version !== 1 || !isRecord(mcp) || typeof mcp.created !== 'boolean' || !isRecord(mcp.servers)
    || !isRecord(skills) || typeof skills.rootCreated !== 'boolean' || !isRecord(skills.links)) {
    throw new Error(`${file} has an unsupported manifest shape`);
  }
  for (const entry of Object.values(mcp.servers)) if (!isRecord(entry) || !('config' in entry)) throw new Error(`${file} has an invalid MCP ownership entry`);
  for (const entry of Object.values(skills.links)) if (!isRecord(entry) || typeof entry.target !== 'string') throw new Error(`${file} has an invalid skill ownership entry`);
  const manifest: RuntimeManifest = value as unknown as RuntimeManifest; // Shape guarded field by field above.
  return manifest;
}

async function saveManifest(configDir: string, manifest: RuntimeManifest): Promise<void> {
  await writeAtomicIfChanged(path.join(configDir, MANIFEST), `${JSON.stringify(manifest, null, 2)}\n`);
}

async function readJsonObject(file: string): Promise<Record<string, unknown> | undefined> {
  const text = await readText(file);
  if (text === undefined) return undefined;
  let value: unknown;
  try {
    value = parseJson(text);
  } catch {
    throw new Error(`${file} is not valid JSON; refusing to rewrite it`);
  }
  if (!isRecord(value)) throw new Error(`${file} is not a JSON object`);
  return value;
}

async function readLooseJson(file: string): Promise<unknown> {
  const text = await readText(file);
  if (text === undefined) return undefined;
  try {
    return parseJson(text);
  } catch {
    return undefined;
  }
}

async function readLooseYaml(file: string): Promise<unknown> {
  const text = await readText(file);
  return text === undefined ? undefined : parseYaml(text);
}

// Bun.YAML is available at runtime but missing from the installed @types/bun.
const yamlParser = (Bun as unknown as { YAML?: { parse(source: string): unknown } }).YAML;
// Bun.JSONC is available at runtime but missing from the installed @types/bun.
const jsoncParser = (Bun as unknown as { JSONC?: { parse(source: string): unknown } }).JSONC;

function parseJson(source: string): unknown {
  return jsoncParser === undefined ? JSON.parse(source) : jsoncParser.parse(source);
}

function parseYaml(source: string): unknown {
  try {
    return yamlParser?.parse(source);
  } catch {
    return undefined;
  }
}

async function readText(file: string): Promise<string | undefined> {
  try {
    return await readFile(file, 'utf8');
  } catch (error) {
    if (isMissing(error)) return undefined;
    throw error;
  }
}

async function writeAtomicIfChanged(file: string, content: string): Promise<boolean> {
  const existing = await readText(file);
  if (existing === content) return false;
  await mkdir(path.dirname(file), { recursive: true });
  const temporary = `${file}.${randomUUID()}.tmp`;
  await writeFile(temporary, content, 'utf8');
  await rename(temporary, file);
  return true;
}

function isMissing(error: unknown): boolean {
  return error instanceof Error && 'code' in error && error.code === 'ENOENT';
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function jsonEqual(left: unknown, right: unknown): boolean {
  if (left === right) return true;
  if (typeof left !== typeof right || left === null || right === null) return false;
  if (Array.isArray(left)) {
    if (!Array.isArray(right) || left.length !== right.length) return false;
    return left.every((entry, index) => jsonEqual(entry, right[index]));
  }
  if (typeof left === 'object') {
    if (Array.isArray(right) || typeof right !== 'object') return false;
    const leftKeys = Object.keys(left);
    if (leftKeys.length !== Object.keys(right).length) return false;
    return leftKeys.every(key => key in right && jsonEqual(Reflect.get(left, key), Reflect.get(right, key)));
  }
  return false;
}
