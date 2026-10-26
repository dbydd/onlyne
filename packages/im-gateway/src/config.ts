import { OnlyneError, type Json, type NodeId } from '@onlyne/lib';

/** One configured connector: which platform, which resource it backs, and its conversation coordinate. */
export interface ConnectorConfig {
  platform: string;
  resource_ref: { owner?: NodeId; id: string };
  /** The conversation coordinate the resource binds to (lark `oc_`/`ou_`, QQ `group:`/`c2c:`). */
  conversation: string;
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
}

const ENV_PREFIX = 'ONLYNE_';
/** `ONLYNE_NODE_ID` and friends override the JSON file for the fields an operator retunes per host. */
function envOverride(container: Record<string, unknown>, field: string, env: Record<string, string | undefined>): void {
  const value = env[ENV_PREFIX + field.toUpperCase()];
  if (value !== undefined) container[field] = value;
}

function requireObject(value: Json | undefined, field: string): Record<string, Json> {
  if (value === undefined || value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new OnlyneError('invalid_request', `The configuration needs ${field} as an object.`);
  }
  return value as Record<string, Json>;
}

function requireString(container: Record<string, unknown>, field: string): string {
  const value = container[field];
  if (typeof value !== 'string' || value.length === 0) {
    throw new OnlyneError('invalid_request', `The configuration needs ${field} as a non-empty string.`);
  }
  return value;
}

function numberOrDefault(container: Record<string, unknown>, field: string, fallback: number): number {
  const value = container[field];
  return typeof value === 'number' && Number.isFinite(value) ? value : fallback;
}

function parseConnectors(raw: Json | undefined, owner: NodeId): ConnectorConfig[] {
  if (raw === undefined) return [];
  if (!Array.isArray(raw)) throw new OnlyneError('invalid_request', 'The configuration needs connectors as an array.');
  return raw.map((entry, index) => {
    const item = requireObject(entry, `connectors[${index}]`);
    const ref = requireObject(item.resource_ref, `connectors[${index}].resource_ref`);
    return {
      platform: requireString(item, 'platform'),
      resource_ref: { owner: typeof ref.owner === 'string' && ref.owner.length > 0 ? ref.owner : owner, id: requireString(ref, 'id') },
      conversation: requireString(item, 'conversation'),
      ...(typeof item.assignee === 'string' && item.assignee.length > 0 ? { assignee: item.assignee } : {}),
      ...(item.script === undefined ? {} : { script: item.script }),
      ...(typeof item.credential === 'string' && item.credential.length > 0 ? { credential: item.credential } : {}),
      ...(typeof item.credential_file === 'string' && item.credential_file.length > 0 ? { credential_file: item.credential_file } : {}),
    };
  });
}

/** Loads the gateway configuration: the JSON file is the base, `ONLYNE_*` environment variables override. */
export async function loadConfig(path: string, env: Record<string, string | undefined> = {}): Promise<GatewayConfig> {
  let file: Json | undefined;
  try {
    file = JSON.parse(await Bun.file(path).text()) as Json;
  } catch {
    throw new OnlyneError('invalid_request', `The configuration file is unreadable: ${path}`);
  }
  const raw = requireObject(file, 'the configuration root');
  const node = requireObject(raw.node, 'node');
  const server = requireObject(raw.server, 'server');
  const storage = requireObject(raw.storage, 'storage');
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
    connectors: parseConnectors(raw.connectors, owner),
    ...(Array.isArray(raw.peers)
      ? { peers: raw.peers.filter((entry): entry is string => typeof entry === 'string' && entry.length > 0) }
      : {}),
  };
}
