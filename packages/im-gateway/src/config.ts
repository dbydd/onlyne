import { OnlyneError, type Json, type NodeId } from '@onlyne/lib';
import type { Locale } from '@onlyne/node-host/i18n';
import { t } from './gateway-text.js';

/** How a channel receives platform events. QQ offers both paths; the other platforms offer one. */
export type ChannelTransport = 'websocket' | 'webhook';

const TRANSPORTS: readonly ChannelTransport[] = ['websocket', 'webhook'];
/** The one connector that speaks a platform gateway socket today. */
const GATEWAY_PLATFORMS: Record<string, true> = { qq: true };

/**
 * One channel: this process serves exactly one platform, on one port, over one store and one
 * node identity (RFC 0012 §2). Every platform conversation it serves becomes a resource of its
 * own, and `conversation` is only the pre-binding for the one that exists from startup.
 */
export interface ChannelConfig {
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
  /** An explicit platform credential (lark `app_id:app_secret`, qq `appid:secret`, wechat bot token). */
  credential?: string;
  /** A credential file path, resolved under `credentials_dir` when relative. */
  credential_file?: string;
  /**
   * The inbound receive path. Defaults to `websocket` on QQ — the platform's own gateway — and is
   * unset elsewhere, where the platform has a single transport. `webhook` serves the signed
   * callback instead, which is what `webhook_path` addresses.
   */
  transport?: ChannelTransport;
  /** The HTTP path this channel's inbound webhook answers on. Default `/im/webhook`. */
  webhook_path?: string;
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
  channel: ChannelConfig;
  /** Seed endpoints to observe at startup; discovery itself stays in lib. */
  peers?: string[];
  /**
   * The aliases the node publishes in its execution context directory (`GET /contexts`), which
   * a sender's draft send reads to continue work here. Unset publishes `["main"]`; an explicit
   * empty array publishes an empty directory.
   */
  contexts?: string[];
  /** The language of the gateway's own console text; unset follows `ONLYNE_LANG`. */
  language?: Locale;
}

export const DEFAULT_WEBHOOK_PATH = '/im/webhook';
/** A platform name prefixes every resource id (RFC 0012 §2), so it cannot carry a separator. */
const PLATFORM_NAME = /^[a-z0-9]+$/;
const ENV_PREFIX = 'ONLYNE_';

/**
 * RFC 0012 §1: one platform per node. The identity is derived from the channel, so an operator
 * cannot point a lark gateway at a node identity that says qq and inherit another channel's trust.
 * A suffixed identity (`urn:onlyne:gateway:im-fake-a`) is the one deviation: it lets a fixture or
 * a test run several instances of the same platform with distinct identities.
 */
export function gatewayNodeId(platform: string): NodeId {
  return `urn:onlyne:gateway:im-${platform}`;
}

function parseNodeId(value: Json | undefined, platform: string): NodeId {
  const conventional = gatewayNodeId(platform);
  if (value === undefined) return conventional;
  if (typeof value !== 'string' || value.length === 0) {
    throw new OnlyneError('invalid_request', t('config.string', { field: 'node.id' }));
  }
  if (value !== conventional && !value.startsWith(`${conventional}-`)) {
    throw new OnlyneError('invalid_request', t('config.nodeIdentity', { node: value, platform, expected: conventional }));
  }
  return value;
}

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

function parseChannel(raw: Json | undefined): ChannelConfig {
  const item = requireObject(raw, 'channel');
  const platform = requireString(item, 'platform');
  if (!PLATFORM_NAME.test(platform)) {
    throw new OnlyneError('invalid_request', t('config.platformName', { platform }));
  }
  const transport = parseTransport(item.transport, platform);
  const webhook = item.webhook_path;
  if (webhook !== undefined && (typeof webhook !== 'string' || !webhook.startsWith('/'))) {
    throw new OnlyneError('invalid_request', t('config.webhookPath', { value: JSON.stringify(webhook) }));
  }
  return {
    platform,
    ...(typeof item.conversation === 'string' && item.conversation.length > 0 ? { conversation: item.conversation } : {}),
    ...(typeof item.assignee === 'string' && item.assignee.length > 0 ? { assignee: item.assignee } : {}),
    ...(item.script === undefined ? {} : { script: item.script }),
    ...(typeof item.credential === 'string' && item.credential.length > 0 ? { credential: item.credential } : {}),
    ...(typeof item.credential_file === 'string' && item.credential_file.length > 0 ? { credential_file: item.credential_file } : {}),
    ...(transport === undefined ? {} : { transport }),
    ...(webhook === undefined ? {} : { webhook_path: webhook }),
  };
}

/**
 * A platform that has no gateway socket would ignore `websocket` silently, so the value is
 * refused instead of parsed and dropped.
 */
function parseTransport(value: Json | undefined, platform: string): ChannelTransport | undefined {
  if (value === undefined) return GATEWAY_PLATFORMS[platform] === true ? 'websocket' : undefined;
  if (value !== 'websocket' && value !== 'webhook') {
    throw new OnlyneError('invalid_request', t('config.transport', {
      accepted: TRANSPORTS.join(', '), value: JSON.stringify(value),
    }));
  }
  if (value === 'websocket' && GATEWAY_PLATFORMS[platform] !== true) {
    throw new OnlyneError('invalid_request', t('config.transportPlatform', { platform, accepted: 'webhook' }));
  }
  return value;
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
  // The aggregate gateway is retired: one process no longer serves several platforms, and a
  // configuration still asking for it must be split by hand instead of half-migrated.
  if (raw.connectors !== undefined) {
    throw new OnlyneError('invalid_request', t('config.aggregateRetired'));
  }
  const node = raw.node === undefined ? {} : requireObject(raw.node, 'node');
  const server = requireObject(raw.server, 'server');
  const storage = requireObject(raw.storage, 'storage');
  const language = parseLanguage(raw.language);
  const channel = parseChannel(raw.channel);
  envOverride(node, 'id', env);
  envOverride(server, 'bind', env);
  envOverride(server, 'port', env);
  const owner = parseNodeId(node.id, channel.platform);
  return {
    node: { id: owner },
    server: {
      bind: typeof server.bind === 'string' && server.bind.length > 0 ? server.bind : '127.0.0.1',
      port: numberOrDefault(server, 'port', 0),
    },
    storage_path: requireString(storage, 'path'),
    ...(typeof raw.credentials_dir === 'string' && raw.credentials_dir.length > 0 ? { credentials_dir: raw.credentials_dir } : {}),
    ...(typeof raw.media_dir === 'string' && raw.media_dir.length > 0 ? { media_dir: raw.media_dir } : {}),
    channel,
    ...(Array.isArray(raw.peers)
      ? { peers: raw.peers.filter((entry): entry is string => typeof entry === 'string' && entry.length > 0) }
      : {}),
    ...(Array.isArray(raw.contexts)
      ? { contexts: [...new Set(raw.contexts.filter((entry): entry is string => typeof entry === 'string' && entry.length > 0))] }
      : {}),
    ...(language === undefined ? {} : { language }),
  };
}
