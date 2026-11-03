import { OnlyneError, type Json, type NodeId } from '@onlyne/lib';
import type { Locale } from '@onlyne/node-host/i18n';
import { t } from './server-text.ts';

export const DEFAULT_NODE_ID: NodeId = 'urn:onlyne:agent:mcp';

export interface McpServerConfig {
  node: { id: NodeId };
  server: { bind: string; port: number };
  storage_path: string;
  peers?: string[];
  contexts?: string[];
  language?: Locale;
}

const ENV_PREFIX = 'ONLYNE_';

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

const parseLanguage = (value: Json | undefined): Locale | undefined => {
  if (value === undefined) return undefined;
  if (value === 'en' || value === 'zh-CN') return value;
  throw new OnlyneError('invalid_request', t('config.language', { value: JSON.stringify(value) }));
};

const portNumber = (value: unknown, fallback: number): number => {
  if (value === undefined) return fallback;
  const parsed = typeof value === 'string' ? Number(value) : value;
  if (typeof parsed === 'number' && Number.isInteger(parsed) && parsed >= 0 && parsed <= 65535) return parsed;
  throw new OnlyneError('invalid_request', t('config.positiveInt', { field: 'server.port' }));
};

function envOverride(container: Record<string, unknown>, field: string, env: Record<string, string | undefined>): void {
  const value = env[ENV_PREFIX + field.toUpperCase()];
  if (value !== undefined) container[field] = value;
}

/** Loads the MCP server configuration: the JSON file is the base, `ONLYNE_*` overrides. */
export async function loadConfig(path: string, env: Record<string, string | undefined> = process.env): Promise<McpServerConfig> {
  let file: Json | undefined;
  try {
    file = JSON.parse(await Bun.file(path).text()) as Json;
  } catch {
    throw new OnlyneError('invalid_request', t('config.unreadable', { path }));
  }
  const raw = requireObject(file, 'the configuration root');
  const node = raw.node === undefined ? {} : requireObject(raw.node, 'node');
  const server = requireObject(raw.server, 'server');
  const storage = requireObject(raw.storage, 'storage');
  const language = parseLanguage(raw.language);
  envOverride(node, 'id', env);
  envOverride(server, 'bind', env);
  envOverride(server, 'port', env);
  if (env.ONLYNE_NODE_ID !== undefined) node.id = env.ONLYNE_NODE_ID;
  if (env.ONLYNE_BIND !== undefined) server.bind = env.ONLYNE_BIND;
  if (env.ONLYNE_PORT !== undefined) server.port = env.ONLYNE_PORT;
  const owner = typeof node.id === 'string' && node.id.length > 0 ? node.id : DEFAULT_NODE_ID;
  return {
    node: { id: owner },
    server: {
      bind: typeof server.bind === 'string' && server.bind.length > 0 ? server.bind : '127.0.0.1',
      port: portNumber(server.port, 0),
    },
    storage_path: requireString(storage, 'path'),
    ...(Array.isArray(raw.peers)
      ? { peers: raw.peers.filter((entry): entry is string => typeof entry === 'string' && entry.length > 0) }
      : {}),
    ...(Array.isArray(raw.contexts)
      ? { contexts: [...new Set(raw.contexts.filter((entry): entry is string => typeof entry === 'string' && entry.length > 0))] }
      : {}),
    ...(language === undefined ? {} : { language }),
  };
}
