import { OnlyneError, type Json, type NodeId } from '@onlyne/lib';
import type { Locale } from '@onlyne/node-host/i18n';
import { t } from './gateway-text.js';

/** One configured MCP server: its resource name, URL and per-call settings. */
export interface ServerConfig {
  name: string;
  url: string;
  /** Extra request headers (an API key, a bank header); never logged. */
  headers?: Record<string, string>;
  timeout_ms?: number;
  /** Who may call this server's tools. Empty means the node owner only. */
  controllers?: NodeId[];
}

export interface GatewayConfig {
  node: { id: NodeId };
  server: { bind: string; port: number };
  storage_path: string;
  servers: ServerConfig[];
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

/** `language` is exactly `en` or `zh-CN`: a near miss would silently pick a different console language. */
const parseLanguage = (value: Json | undefined): Locale | undefined => {
  if (value === undefined) return undefined;
  if (value === 'en' || value === 'zh-CN') return value;
  throw new OnlyneError('invalid_request', t('config.language', { value: JSON.stringify(value) }));
};

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
  const servers = rawServers.map((entry, index): ServerConfig => {
    const item = requireObject(entry, `servers[${index}]`);
    const rawHeaders = item.headers === undefined ? undefined : requireObject(item.headers, `servers[${index}].headers`);
    const headers = rawHeaders === undefined
      ? undefined
      : Object.fromEntries(Object.entries(rawHeaders).filter((pair): pair is [string, string] => typeof pair[1] === 'string'));
    const timeout = item.timeout_ms;
    const controllers = item.controllers;
    return {
      name: requireString(item, 'name'),
      url: requireString(item, 'url'),
      ...(headers === undefined || Object.keys(headers).length === 0 ? {} : { headers }),
      ...(typeof timeout === 'number' ? { timeout_ms: timeout } : {}),
      ...(Array.isArray(controllers)
        ? { controllers: controllers.filter((value): value is NodeId => typeof value === 'string' && value.length > 0) }
        : {}),
    };
  });
  const names = new Set<string>();
  for (const entry of servers) {
    if (names.has(entry.name)) throw new OnlyneError('invalid_request', t('config.duplicateName', { name: entry.name }));
    names.add(entry.name);
  }
  return {
    node: { id: owner },
    server: {
      bind: typeof server.bind === 'string' && server.bind.length > 0 ? server.bind : '127.0.0.1',
      port: typeof server.port === 'number' ? server.port : 0,
    },
    storage_path: requireString(storage, 'path'),
    servers,
    ...(language === undefined ? {} : { language }),
  };
}
