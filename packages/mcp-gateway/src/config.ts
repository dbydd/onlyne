import { OnlyneError, type Json, type NodeId } from '@onlyne/lib';
import type { Locale } from '@onlyne/node-host/i18n';
import { t } from './gateway-text.js';
import { MOUNTS_RESOURCE_PREFIX } from './mounts.js';

/** Settings every configured MCP server shares. */
interface ServerConfigBase {
  name: string;
  timeout_ms?: number;
  /** Who may call this server's tools. Empty means the node owner only. */
  controllers?: NodeId[];
  /**
   * The local tool policy (RFC 0015 §4). Absent allows every tool the server lists; an empty
   * list allows none. It is checked before a request leaves the gateway.
   */
  allowed_tools?: string[];
}

/** An HTTP streamable MCP server; the transport is implicit when only `url` is given. */
export interface HttpServerConfig extends ServerConfigBase {
  transport?: 'http';
  url: string;
  /** Extra request headers (an API key, a bank header); never logged. */
  headers?: Record<string, string>;
}

/** A locally spawned MCP server; `command` and `args` are spawned directly, never through a shell. */
export interface StdioServerConfig extends ServerConfigBase {
  transport: 'stdio';
  command: string;
  args?: string[];
  env?: Record<string, string>;
  cwd?: string;
}

/** One configured MCP server: exactly one of the HTTP and stdio transport contracts. */
export type ServerConfig = HttpServerConfig | StdioServerConfig;

/** One locally allowlisted executable a scheduled mount may run: the mount names this id only. */
export interface MountCommandConfig {
  id: string;
  command: string;
  args?: string[];
  cwd?: string;
}

/** One MCP tool a scheduled mount may call; the server's own controller list still applies. */
export interface MountToolConfig { server: string; tool: string }

export interface MountsConfig {
  /** The nodes that may mount, inspect and cancel tasks on their own scheduler resource. */
  controllers: NodeId[];
  commands: MountCommandConfig[];
  tools: MountToolConfig[];
  tick_ms: number;
}

export interface GatewayConfig {
  node: { id: NodeId };
  server: { bind: string; port: number };
  storage_path: string;
  servers: ServerConfig[];
  /** Scheduled mounts; absent means the gateway serves no scheduler resource at all. */
  mounts?: MountsConfig;
  /** The language of the gateway's own console text; unset follows `ONLYNE_LANG`. */
  language?: Locale;
}

const requireObject = (value: Json | undefined, field: string): Record<string, Json> => {
  if (value === undefined || value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new OnlyneError('invalid_request', t('config.object', { field }));
  }
  return value as Record<string, Json>;
};

const requireString = (container: Record<string, unknown>, field: string): string => {
  const value = container[field];
  if (typeof value !== 'string' || value.length === 0) {
    throw new OnlyneError('invalid_request', t('config.string', { field }));
  }
  return value;
};

const requireStrings = (value: Json | undefined, field: string): string[] => {
  if (value === undefined) return [];
  if (!Array.isArray(value) || value.some(item => typeof item !== 'string' || item.length === 0)) {
    throw new OnlyneError('invalid_request', t('config.stringList', { field }));
  }
  return value as string[];
};

function parseMounts(raw: Json | undefined, owner: NodeId, servers: ReadonlyMap<string, ServerConfig>): MountsConfig | undefined {
  if (raw === undefined) return undefined;
  const record = requireObject(raw, 'mounts');
  const controllers = [...new Set(requireStrings(record.controllers, 'mounts.controllers'))];
  if (controllers.length === 0) {
    throw new OnlyneError('invalid_request', t('config.mountsControllers'));
  }
  if (controllers.includes(owner)) {
    throw new OnlyneError('invalid_request', t('config.mountsOwner', { owner }));
  }
  const commands = (record.commands === undefined ? [] : record.commands);
  if (!Array.isArray(commands)) throw new OnlyneError('invalid_request', t('config.mountsCommands'));
  const commandIds = new Set<string>();
  const parsedCommands = commands.map((entry, index): MountCommandConfig => {
    const item = requireObject(entry, `mounts.commands[${index}]`);
    const id = requireString(item, 'id');
    if (commandIds.has(id)) throw new OnlyneError('invalid_request', t('config.mountsDuplicate', { kind: 'command', id }));
    commandIds.add(id);
    const args = item.args === undefined ? [] : requireStrings(item.args, `mounts.commands[${index}].args`);
    const cwd = item.cwd === undefined ? undefined : requireString(item, 'cwd');
    return {
      id,
      command: requireString(item, 'command'),
      ...(args.length === 0 ? {} : { args }),
      ...(cwd === undefined ? {} : { cwd }),
    };
  });
  const tools = (record.tools === undefined ? [] : record.tools);
  if (!Array.isArray(tools)) throw new OnlyneError('invalid_request', t('config.mountsTools'));
  const toolIds = new Set<string>();
  const parsedTools = tools.map((entry, index): MountToolConfig => {
    const item = requireObject(entry, `mounts.tools[${index}]`);
    const server = requireString(item, 'server');
    const tool = requireString(item, 'tool');
    const configured = servers.get(server);
    if (configured === undefined) {
      throw new OnlyneError('invalid_request', t('config.mountsToolServer', { server }));
    }
    // A scheduled call runs with the same local policy as a direct resource operation; refusing
    // a tool the policy already denies at load time keeps the scheduler from starting a task
    // that can only fail.
    if (configured.allowed_tools !== undefined && !configured.allowed_tools.includes(tool)) {
      throw new OnlyneError('invalid_request', `mounts.tools names ${server}/${tool}, which the server's allowed_tools policy denies.`);
    }
    const pair = `${server}\u0000${tool}`;
    if (toolIds.has(pair)) throw new OnlyneError('invalid_request', t('config.mountsDuplicate', { kind: 'tool', id: `${server}/${tool}` }));
    toolIds.add(pair);
    return { server, tool };
  });
  const tick = record.tick_ms === undefined ? 1_000 : record.tick_ms;
  if (typeof tick !== 'number' || !Number.isInteger(tick) || tick < 100 || tick > 60_000) {
    throw new OnlyneError('invalid_request', t('config.mountTick'));
  }
  return { controllers, commands: parsedCommands, tools: parsedTools, tick_ms: tick };
}

/** `language` is exactly `en` or `zh-CN`: a near miss would silently pick a different console language. */
const parseLanguage = (value: Json | undefined): Locale | undefined => {
  if (value === undefined) return undefined;
  if (value === 'en' || value === 'zh-CN') return value;
  throw new OnlyneError('invalid_request', t('config.language', { value: JSON.stringify(value) }));
};

/** Environment entries are opaque strings; an empty value is legal, a non-string is not. */
function parseEnvironment(value: Json | undefined, field: string): Record<string, string> | undefined {
  if (value === undefined) return undefined;
  const record = requireObject(value, field);
  const environment: Record<string, string> = {};
  for (const [key, entry] of Object.entries(record)) {
    if (typeof entry !== 'string') {
      throw new OnlyneError('invalid_request', t('config.string', { field: `${field}.${key}` }));
    }
    environment[key] = entry;
  }
  return Object.keys(environment).length === 0 ? undefined : environment;
}

/** One `servers[]` entry: the transport decides which fields are required and which are refused. */
function parseServer(item: Record<string, Json>, index: number): ServerConfig {
  const field = `servers[${index}]`;
  const timeout = item.timeout_ms;
  const rawAllowed = item.allowed_tools;
  const allowed = rawAllowed === undefined
    ? undefined
    : [...new Set(requireStrings(rawAllowed, `${field}.allowed_tools`))];
  const controllers = item.controllers;
  const shared = {
    name: requireString(item, 'name'),
    ...(typeof timeout === 'number' ? { timeout_ms: timeout } : {}),
    ...(allowed === undefined ? {} : { allowed_tools: allowed }),
    ...(Array.isArray(controllers)
      ? { controllers: controllers.filter((value): value is NodeId => typeof value === 'string' && value.length > 0) }
      : {}),
  };
  const declared = item.transport;
  if (declared !== undefined && declared !== 'http' && declared !== 'stdio') {
    throw new OnlyneError('invalid_request', `${field}.transport is 'http' or 'stdio'.`);
  }
  // An existing configuration names only `url`; that stays the HTTP transport without a rewrite.
  const transport = declared ?? (item.url !== undefined ? 'http' : undefined);
  if (transport === 'http') {
    for (const key of ['command', 'args', 'env', 'cwd'] as const) {
      if (item[key] !== undefined) {
        throw new OnlyneError('invalid_request', `${field} uses the http transport, so it cannot set ${key}.`);
      }
    }
    const rawHeaders = item.headers === undefined ? undefined : requireObject(item.headers, `${field}.headers`);
    const headers = rawHeaders === undefined
      ? undefined
      : Object.fromEntries(Object.entries(rawHeaders).filter((pair): pair is [string, string] => typeof pair[1] === 'string'));
    return {
      ...shared,
      transport: 'http',
      url: requireString(item, 'url'),
      ...(headers === undefined || Object.keys(headers).length === 0 ? {} : { headers }),
    };
  }
  if (transport === 'stdio') {
    if (item.url !== undefined) {
      throw new OnlyneError('invalid_request', `${field} uses the stdio transport, so it cannot set url.`);
    }
    if (item.headers !== undefined) {
      throw new OnlyneError('invalid_request', `${field} uses the stdio transport, so it cannot set headers.`);
    }
    const args = item.args === undefined ? [] : requireStrings(item.args, `${field}.args`);
    const env = parseEnvironment(item.env, `${field}.env`);
    const cwd = item.cwd === undefined ? undefined : requireString(item, 'cwd');
    return {
      ...shared,
      transport: 'stdio',
      command: requireString(item, 'command'),
      ...(args.length === 0 ? {} : { args }),
      ...(env === undefined ? {} : { env }),
      ...(cwd === undefined ? {} : { cwd }),
    };
  }
  throw new OnlyneError('invalid_request', `${field} needs transport 'http' with url, or transport 'stdio' with command.`);
}

/** Loads the gateway configuration from one JSON file; environment retuning comes later. */
export async function loadConfig(path: string): Promise<GatewayConfig> {
  let file: Json | undefined;
  try {
    file = JSON.parse(await Bun.file(path).text()) as Json;
  } catch {
    throw new OnlyneError('invalid_request', t('config.unreadable', { path }));
  }
  const raw = requireObject(file, 'the configuration root');
  const node = requireObject(raw.node, 'node');
  const server = requireObject(raw.server, 'server');
  const storage = requireObject(raw.storage, 'storage');
  const language = parseLanguage(raw.language);
  const owner = requireString(node, 'id') as NodeId;
  const rawServers = raw.servers === undefined ? [] : raw.servers;
  if (!Array.isArray(rawServers)) {
    throw new OnlyneError('invalid_request', t('config.servers'));
  }
  const servers = rawServers.map((entry, index) => parseServer(requireObject(entry, `servers[${index}]`), index));
  const names = new Set<string>();
  for (const entry of servers) {
    if (names.has(entry.name)) throw new OnlyneError('invalid_request', t('config.duplicateName', { name: entry.name }));
    if (entry.name.startsWith(MOUNTS_RESOURCE_PREFIX)) {
      throw new OnlyneError('invalid_request', t('config.reservedName', { name: entry.name, prefix: MOUNTS_RESOURCE_PREFIX }));
    }
    names.add(entry.name);
  }
  const mounts = parseMounts(raw.mounts, owner, new Map(servers.map(entry => [entry.name, entry])));
  return {
    node: { id: owner },
    server: {
      bind: typeof server.bind === 'string' && server.bind.length > 0 ? server.bind : '127.0.0.1',
      port: typeof server.port === 'number' ? server.port : 0,
    },
    storage_path: requireString(storage, 'path'),
    servers,
    ...(mounts === undefined ? {} : { mounts }),
    ...(language === undefined ? {} : { language }),
  };
}
