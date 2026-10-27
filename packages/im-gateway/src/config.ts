import { OnlyneError, type Json, type NodeId } from '@onlyne/lib';
import type { Locale } from '@onlyne/node-host/i18n';
import { t } from './gateway-text.js';

/**
 * One configured connector: one platform connection and one credential (RFC 0012 §2). It backs no
 * single resource — every platform conversation it serves becomes a resource of its own, and
 * `conversation` is only the pre-binding for the one that exists from startup.
 */
export interface ConnectorConfig {
  platform: string;
  /**
   * Pre-binding, optional (RFC 0012 §2): this conversation is adopted at startup, with the same
   * result an adopt produces. Omitted, every conversation of this connector adopts on first contact.
   */
  conversation?: string;
  /** The agent node the resource is assigned to; its event recipient and the push target. */
  assignee?: NodeId;
  /** im-fake only: the scripted inbound events and send answers, passed through untyped. */
  script?: unknown;
  /** An explicit platform credential (`app_id:app_secret`). */
  credential?: string;
  /** A credential file path, resolved under `credentials_dir` when relative. */
  credential_file?: string;
}

export interface GatewayConfig {
  node: { id: NodeId };
  server: { bind: string; port: number };
  /** Absolute or config-relative path of the on-disk store. */
  storage_path: string;
  /** Directory connector credential files resolve in. */
  credentials_dir?: string;
  /** Directory inbound attachments are materialized under (RFC 0012 §9); unset keeps handles only. */
  media_dir?: string;
  connectors: ConnectorConfig[];
  /** Seed endpoints to observe at startup; discovery itself stays in lib. */
  peers?: string[];
  /** The language of the gateway's own console text; unset follows `ONLYNE_LANG`. */
  language?: Locale;
}

const ENV_PREFIX = 'ONLYNE_';
/** `ONLYNE_NODE_ID` and friends override the JSON file for the fields an operator retunes per host. */
function envOverride(container: Record<string, unknown>, field: string, env: Record<string, string | undefined>): void {
  const value = env[ENV_PREFIX + field.toUpperCase()];
  if (value !== undefined) container[field] = value;
}

function requireObject(value: Json | undefined, field: string): Record<string, Json> {
  if (value === undefined || value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new OnlyneError('invalid_request', t('config.object', { field }));
  }
  return value as Record<string, Json>;
}

function requireString(container: Record<string, unknown>, field: string): string {
  const value = container[field];
  if (typeof value !== 'string' || value.length === 0) {
    throw new OnlyneError('invalid_request', t('config.string', { field }));
  }
  return value;
}

function numberOrDefault(container: Record<string, unknown>, field: string, fallback: number): number {
  const value = container[field];
  return typeof value === 'number' && Number.isFinite(value) ? value : fallback;
}

function parseConnectors(raw: Json | undefined): ConnectorConfig[] {
  if (raw === undefined) return [];
  if (!Array.isArray(raw)) throw new OnlyneError('invalid_request', t('config.connectors'));
  const connectors = raw.map((entry, index) => {
    const item = requireObject(entry, `connectors[${index}]`);
    return {
      platform: requireString(item, 'platform'),
      ...(typeof item.conversation === 'string' && item.conversation.length > 0 ? { conversation: item.conversation } : {}),
      ...(typeof item.assignee === 'string' && item.assignee.length > 0 ? { assignee: item.assignee } : {}),
      ...(item.script === undefined ? {} : { script: item.script }),
      ...(typeof item.credential === 'string' && item.credential.length > 0 ? { credential: item.credential } : {}),
      ...(typeof item.credential_file === 'string' && item.credential_file.length > 0 ? { credential_file: item.credential_file } : {}),
    };
  });
  // One platform is one connection: two entries would share a resource namespace and the bridge
  // would have to guess which credential serves a conversation.
  const seen = new Set<string>();
  for (const connector of connectors) {
    if (seen.has(connector.platform)) throw new OnlyneError('invalid_request', t('config.platformTwice', { platform: connector.platform }));
    seen.add(connector.platform);
  }
  return connectors;
}

/** `language` is exactly `en` or `zh-CN`: a near miss would silently pick a different console language. */
function parseLanguage(value: Json | undefined): Locale | undefined {
  if (value === undefined) return undefined;
  if (value === 'en' || value === 'zh-CN') return value;
  throw new OnlyneError('invalid_request', t('config.language', { value: JSON.stringify(value) }));
}

/** Loads the gateway configuration: the JSON file is the base, `ONLYNE_*` environment variables override. */
export async function loadConfig(path: string, env: Record<string, string | undefined> = {}): Promise<GatewayConfig> {
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
  envOverride(node, 'id', env);
  envOverride(server, 'bind', env);
  envOverride(server, 'port', env);
  const owner = requireString(node, 'id') as NodeId;
  return {
    node: { id: owner },
    server: {
      bind: typeof server.bind === 'string' && server.bind.length > 0 ? server.bind : '127.0.0.1',
      port: numberOrDefault(server, 'port', 0),
    },
    storage_path: requireString(storage, 'path'),
    ...(typeof raw.credentials_dir === 'string' && raw.credentials_dir.length > 0 ? { credentials_dir: raw.credentials_dir } : {}),
    ...(typeof raw.media_dir === 'string' && raw.media_dir.length > 0 ? { media_dir: raw.media_dir } : {}),
    connectors: parseConnectors(raw.connectors),
    ...(Array.isArray(raw.peers)
      ? { peers: raw.peers.filter((entry): entry is string => typeof entry === 'string' && entry.length > 0) }
      : {}),
    ...(language === undefined ? {} : { language }),
  };
}
