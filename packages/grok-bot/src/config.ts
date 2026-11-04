import { OnlyneError, type Json, type NodeId, type TrustSeed } from '@onlyne/lib';
import type { Locale } from '@onlyne/node-host/i18n';
import { t } from './node-text.ts';

export const DEFAULT_HEADER = 'Authorization';
export const DEFAULT_SCHEME = 'Bearer';
export const DEFAULT_REPLY_PATH = '/grok/reply';
export const DEFAULT_STATUS_PATH = '/grok/status';
export const DEFAULT_EVENTS_PATH = '/grok/events';
export const DEFAULT_ACK_PATH = '/grok/events/ack';
export const DEFAULT_TIMEOUT_MS = 10_000;
export const DEFAULT_MAX_ATTEMPTS = 8;
export const DEFAULT_RETRY_BASE_MS = 500;
export const DEFAULT_NODE_ID: NodeId = 'urn:onlyne:gateway:grok-bot';

export interface WebhookConfig {
  url: string;
  /** Sender key placed in the configured header. Never logged. */
  key: string;
  /** HTTP header that carries the sender key. Default `Authorization`. */
  header: string;
  /**
   * Prefix before the key in that header. Default `Bearer`, so the header is
   * `Authorization: Bearer <key>`. An empty string sends the key as the header value with no prefix.
   */
  scheme: string;
  timeout_ms: number;
  max_attempts: number;
  retry_base_ms: number;
}

export interface GrokBotConfig {
  node: { id: NodeId };
  server: { bind: string; port: number };
  storage_path: string;
  webhook: WebhookConfig;
  /** HTTP path Grok Bot posts a reply to. Default `/grok/reply`. */
  reply_path: string;
  /** Long-poll GET for owed wakes. Default `/grok/events`. */
  events_path: string;
  /** POST ack path for pulled wakes. Default `/grok/events/ack`. */
  ack_path: string;
  /**
   * Pinned peers this node may approve without the trust terminal. Each row is a node id
   * plus the 16-hex fingerprint of that peer's current key. A live key that does not match
   * is refused; peers not listed still need a human (or a later pin). There is no accept-all.
   */
  trust_seeds?: TrustSeed[];
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

const positiveInt = (value: unknown, field: string, fallback: number): number => {
  if (value === undefined) return fallback;
  const parsed = typeof value === 'string' ? Number(value) : value;
  if (typeof parsed === 'number' && Number.isInteger(parsed) && parsed > 0) return parsed;
  throw new OnlyneError('invalid_request', t('config.positiveInt', { field }));
};

const portNumber = (value: unknown, fallback: number): number => {
  if (value === undefined) return fallback;
  const parsed = typeof value === 'string' ? Number(value) : value;
  if (typeof parsed === 'number' && Number.isInteger(parsed) && parsed >= 0 && parsed <= 65535) return parsed;
  throw new OnlyneError('invalid_request', t('config.positiveInt', { field: 'server.port' }));
};

/**
 * Expands `$NAME` and `${NAME}` from `env`. `$$` is a literal `$`. A missing name fails
 * instead of expanding to empty, so a webhook never starts with a silently blank secret.
 */
export function expandEnv(value: string, env: Record<string, string | undefined>, field: string): string {
  const missing = (name: string): never => {
    throw new OnlyneError('invalid_request', t('config.envMissing', { name, field }));
  };
  return value
    .replace(/\$\$/g, '\u0000')
    .replace(/\$\{([A-Z_][A-Z0-9_]*)\}/g, (_match, name: string) => {
      const found = env[name];
      if (found === undefined) return missing(name);
      return found;
    })
    .replace(/\$([A-Z_][A-Z0-9_]*)/g, (_match, name: string) => {
      const found = env[name];
      if (found === undefined) return missing(name);
      return found;
    })
    .replace(/\u0000/g, '$');
}

function envOverride(container: Record<string, unknown>, field: string, env: Record<string, string | undefined>): void {
  const value = env[ENV_PREFIX + field.toUpperCase()];
  if (value !== undefined) container[field] = value;
}

function parseUrl(value: string, field: string): string {
  let parsed: URL;
  try {
    parsed = new URL(value);
  } catch {
    throw new OnlyneError('invalid_request', t('config.webhookUrl'));
  }
  if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') {
    throw new OnlyneError('invalid_request', t('config.webhookUrl'));
  }
  if (value.length === 0) throw new OnlyneError('invalid_request', t('config.string', { field }));
  return value;
}

const FINGERPRINT = /^[0-9a-f]{16}$/;

function parseFingerprint(value: unknown, field: string): string {
  if (typeof value !== 'string' || !FINGERPRINT.test(value.trim().toLowerCase())) {
    throw new OnlyneError('invalid_request', t('config.trustFingerprint', { field }));
  }
  return value.trim().toLowerCase();
}

/** Parses pinned `{ node_id, fingerprint }` rows. Missing fingerprint is refused, never a wildcard. */
export function parseTrustSeeds(value: Json | undefined, field = 'trust_seeds'): TrustSeed[] | undefined {
  if (value === undefined) return undefined;
  if (!Array.isArray(value)) throw new OnlyneError('invalid_request', t('config.trustSeeds', { field }));
  const seeds: TrustSeed[] = [];
  const seen = new Set<string>();
  for (const [index, entry] of value.entries()) {
    const object = rec(entry);
    if (object === undefined) throw new OnlyneError('invalid_request', t('config.trustSeeds', { field }));
    const node_id = typeof object.node_id === 'string' ? object.node_id.trim() : '';
    if (node_id.length === 0) {
      throw new OnlyneError('invalid_request', t('config.trustSeeds', { field: `${field}[${index}].node_id` }));
    }
    const fingerprint = parseFingerprint(object.fingerprint, `${field}[${index}].fingerprint`);
    if (seen.has(node_id)) throw new OnlyneError('invalid_request', t('config.trustDuplicate', { node: node_id }));
    seen.add(node_id);
    seeds.push({ node_id, fingerprint });
  }
  return seeds;
}

function rec(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
}

function parseEnvJson(value: string, field: string): Json {
  try {
    return JSON.parse(value) as Json;
  } catch {
    throw new OnlyneError('invalid_request', t('config.trustSeeds', { field }));
  }
}

function parsePath(value: Json | undefined, fallback: string, field: string): string {
  if (value === undefined) return fallback;
  if (typeof value !== 'string' || !value.startsWith('/')) {
    throw new OnlyneError('invalid_request', t('config.replyPath', { value: JSON.stringify(value) }));
  }
  return value;
}

function headerName(value: string): string {
  if (!/^[A-Za-z0-9!#$%&'*+.^_`|~-]+$/.test(value)) {
    throw new OnlyneError('invalid_request', t('config.webhookHeader'));
  }
  return value;
}

function parseWebhook(raw: Json | undefined, env: Record<string, string | undefined>): WebhookConfig {
  const item = raw === undefined ? {} : requireObject(raw, 'webhook');
  if (env.ONLYNE_GROK_WEBHOOK_URL !== undefined) item.url = env.ONLYNE_GROK_WEBHOOK_URL;
  if (env.ONLYNE_GROK_WEBHOOK_KEY !== undefined) item.key = env.ONLYNE_GROK_WEBHOOK_KEY;
  if (env.ONLYNE_GROK_WEBHOOK_HEADER !== undefined) item.header = env.ONLYNE_GROK_WEBHOOK_HEADER;
  if (env.ONLYNE_GROK_WEBHOOK_SCHEME !== undefined) item.scheme = env.ONLYNE_GROK_WEBHOOK_SCHEME;
  if (env.ONLYNE_GROK_WEBHOOK_TIMEOUT_MS !== undefined) item.timeout_ms = env.ONLYNE_GROK_WEBHOOK_TIMEOUT_MS;
  if (env.ONLYNE_GROK_WEBHOOK_MAX_ATTEMPTS !== undefined) item.max_attempts = env.ONLYNE_GROK_WEBHOOK_MAX_ATTEMPTS;
  if (env.ONLYNE_GROK_WEBHOOK_RETRY_BASE_MS !== undefined) item.retry_base_ms = env.ONLYNE_GROK_WEBHOOK_RETRY_BASE_MS;

  const urlRaw = item.url;
  const keyRaw = item.key;
  if (typeof urlRaw !== 'string' || urlRaw.length === 0) {
    throw new OnlyneError('invalid_request', t('config.webhookUrl'));
  }
  if (typeof keyRaw !== 'string' || keyRaw.length === 0) {
    throw new OnlyneError('invalid_request', t('config.webhookKey'));
  }
  const url = parseUrl(expandEnv(urlRaw, env, 'webhook.url'), 'webhook.url');
  const key = expandEnv(keyRaw, env, 'webhook.key');
  if (key.length === 0) throw new OnlyneError('invalid_request', t('config.webhookKey'));

  const headerRaw = item.header === undefined ? DEFAULT_HEADER : item.header;
  if (typeof headerRaw !== 'string' || headerRaw.length === 0) {
    throw new OnlyneError('invalid_request', t('config.webhookHeader'));
  }
  const schemeRaw = item.scheme === undefined ? DEFAULT_SCHEME : item.scheme;
  if (typeof schemeRaw !== 'string') {
    throw new OnlyneError('invalid_request', t('config.webhookScheme'));
  }

  return {
    url,
    key,
    header: headerName(headerRaw),
    scheme: schemeRaw,
    timeout_ms: positiveInt(item.timeout_ms, 'webhook.timeout_ms', DEFAULT_TIMEOUT_MS),
    max_attempts: positiveInt(item.max_attempts, 'webhook.max_attempts', DEFAULT_MAX_ATTEMPTS),
    retry_base_ms: positiveInt(item.retry_base_ms, 'webhook.retry_base_ms', DEFAULT_RETRY_BASE_MS),
  };
}

/** Loads the node configuration: the JSON file is the base, `ONLYNE_*` environment variables override. */
export async function loadConfig(path: string, env: Record<string, string | undefined> = process.env): Promise<GrokBotConfig> {
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
  const trustRaw = env.ONLYNE_GROK_TRUST_SEEDS !== undefined
    ? parseEnvJson(env.ONLYNE_GROK_TRUST_SEEDS, 'ONLYNE_GROK_TRUST_SEEDS')
    : raw.trust_seeds;
  const trust_seeds = parseTrustSeeds(trustRaw);
  return {
    node: { id: owner },
    server: {
      bind: typeof server.bind === 'string' && server.bind.length > 0 ? server.bind : '127.0.0.1',
      port: portNumber(server.port, 0),
    },
    storage_path: requireString(storage, 'path'),
    webhook: parseWebhook(raw.webhook, env),
    reply_path: parsePath(raw.reply_path, DEFAULT_REPLY_PATH, 'reply_path'),
    events_path: parsePath(raw.events_path, DEFAULT_EVENTS_PATH, 'events_path'),
    ack_path: parsePath(raw.ack_path, DEFAULT_ACK_PATH, 'ack_path'),
    ...(trust_seeds === undefined || trust_seeds.length === 0 ? {} : { trust_seeds }),
    ...(Array.isArray(raw.peers)
      ? { peers: raw.peers.filter((entry): entry is string => typeof entry === 'string' && entry.length > 0) }
      : {}),
    ...(Array.isArray(raw.contexts)
      ? { contexts: [...new Set(raw.contexts.filter((entry): entry is string => typeof entry === 'string' && entry.length > 0))] }
      : {}),
    ...(language === undefined ? {} : { language }),
  };
}

/** Builds the Authorization-style header value; the raw key is never returned by itself. */
export function webhookHeaderValue(webhook: Pick<WebhookConfig, 'key' | 'scheme'>): string {
  return webhook.scheme.length === 0 ? webhook.key : `${webhook.scheme} ${webhook.key}`;
}

/** A status view of the webhook that never includes the sender key. */
export function redactWebhook(webhook: WebhookConfig): { url: string; header: string; scheme: string; timeout_ms: number; max_attempts: number; key: 'set' } {
  let url = webhook.url;
  try {
    const parsed = new URL(webhook.url);
    parsed.search = '';
    parsed.hash = '';
    url = `${parsed.origin}${parsed.pathname}`;
  } catch { /* keep the original; loadConfig already validated it */ }
  return {
    url,
    header: webhook.header,
    scheme: webhook.scheme,
    timeout_ms: webhook.timeout_ms,
    max_attempts: webhook.max_attempts,
    key: 'set',
  };
}
