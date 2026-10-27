import {
  OnlyneNode, OnlyneError, type Auth, type KeyHook, type MaintenanceClock, type NodeId, type Plugins, type Resource,
} from '@onlyne/lib';
import { createMcpBridge, resourceOf, McpConnection, type McpServerOptions } from '@onlyne/mcp-core';
import type { GatewayConfig, ServerConfig } from './config.js';
import { FileStorage } from '@onlyne/im-gateway/storage';
import { t } from './gateway-text.js';

export type { GatewayConfig, ServerConfig } from './config.js';

export interface McpGateway {
  readonly node: OnlyneNode;
  readonly config: GatewayConfig;
  /** The MCP connections, one per configured server, keyed by resource id. */
  readonly connections: Map<string, McpConnection>;
  stop(): Promise<void>;
}

/** The Node-timer MaintenanceClock: one-shot timers, the lib's loop re-arms them. */
export function nodeClock(): MaintenanceClock {
  return {
    setTimer: (ms, run) => setTimeout(run, ms),
    clearTimer: handle => clearTimeout(handle as ReturnType<typeof setTimeout>),
  };
}

/**
 * Assembles the gateway: one session per configured MCP server, one Onlyne resource per server.
 * A server that cannot be reached at startup still registers (with its last known tools if the
 * store has them, an empty list otherwise): the node stays up, and the session re-initializes
 * on the first call (RFC 0015 §5).
 */
export interface McpGatewayOptions {
  log?: (message: string, fields?: Record<string, unknown>) => void;
  /**
   * RFC 0017 identity key. The CLI always supplies one; a programmatic caller may omit it and
   * keep the keyless loopback binding the tests use.
   */
  keys?: KeyHook;
}

export async function createMcpGateway(config: GatewayConfig, options: McpGatewayOptions = {}): Promise<McpGateway> {
  const owner: NodeId = config.node.id;
  const storage = new FileStorage(config.storage_path);
  const connections = new Map<string, McpConnection>();
  const serverEntries: Array<{ server: McpServerOptions; connection: McpConnection }> = [];

  for (const entry of config.servers) {
    const server: McpServerOptions = {
      name: entry.name,
      url: entry.url,
      ...(entry.headers === undefined ? {} : { headers: entry.headers }),
      ...(entry.timeout_ms === undefined ? {} : { timeoutMs: entry.timeout_ms }),
    };
    const connection = new McpConnection(server);
    let tools: Awaited<ReturnType<McpConnection['refreshTools']>> = [];
    // The last known tool list rides in the store, so a temporarily unreachable server still
    // presents its operations instead of an empty resource.
    const cached = await storage.transaction(async tx => tx.get<{ tools: typeof tools }>(`mcp/tools/${encodeURIComponent(entry.name)}`));
    try {
      await connection.start();
      tools = connection.tools;
      await storage.transaction(async tx => { await tx.put(`mcp/tools/${encodeURIComponent(entry.name)}`, { tools }); });
    } catch (err) {
      // Registration proceeds with the cache; the connection retries on first call.
      options.log?.('mcp.server.start.failed', { server: entry.name, error: String(err) });
      tools = cached?.tools ?? [];
    }
    connections.set(entry.name, connection);
    serverEntries.push({ server, connection });
  }

  const bridge = createMcpBridge(serverEntries);
  const base_url = `http://${config.server.bind}:${config.server.port}/onlyne/v3`;
  const keys = options.keys;
  const authenticate = async (request: Request): Promise<Auth | null> => {
    const actor = request.headers.get('Onlyne-Actor');
    if (actor === null || actor.length === 0) return null;
    if (actor === owner) throw new OnlyneError('forbidden', t('error.claimIdentity'));
    return { actor };
  };
  const plugins: Omit<Plugins, 'storage' | 'resources'> = {
    authenticate,
    maintenanceClock: nodeClock(),
    resolve: async id => {
      // A peer's address is whatever discovery last verified; the actor header is what names
      // this node on the wire, and the signing transport adds the signature over it.
      const target = id === owner ? { node_id: id, base_url } : await node.discovery.resolve(id);
      return { ...target, headers: { 'Onlyne-Actor': owner } };
    },
    decide: () => { throw new OnlyneError('decision_unavailable', t('error.decide')); },
    runtime: {
      // The terminal log is the gateway's only console, so every exchange stage lands in it
      // with the fingerprint the operator has to repeat on `confirm`.
      notify: event => {
        if (event.event !== 'trust') return;
        options.log?.('onlyne.trust', {
          node: event.entry.node_id, phase: event.phase, fingerprint: event.entry.fingerprint, cause: event.cause,
        });
      },
      supports: async () => false,
      inject: async () => { throw new OnlyneError('unsupported_delivery', t('error.noContext')); },
    },
    domainCandidates: async domain_id => node.discovery.candidates(domain_id),
    ...(keys === undefined ? {} : { keys }),
  };
  let node: OnlyneNode;
  node = new OnlyneNode({
    node_id: owner,
    base_url,
    // RFC 0017 §1: a keyed node advertises the scheme it authenticates with. The keyless
    // binding stays for programmatic callers on loopback.
    authentication: keys === undefined ? ['none'] : ['signature'],
    plugins: { ...plugins, storage, resources: bridge },
    now: () => Date.now(),
    id: () => crypto.randomUUID(),
  });

  const gatewayAuth: Auth = { actor: owner };
  for (const [index, entry] of config.servers.entries()) {
    const controllers = Array.from(new Set([owner, ...(entry.controllers ?? [])]));
    const definition = resourceOf(owner, serverEntries[index]!.server, serverEntries[index]!.connection.tools, controllers);
    // RFC 0006 §3: one handler operates a resource, so the first configured controller holds
    // the control right; every other controller keeps read access through the log reader list.
    const resource: Resource = {
      resource_ref: definition.resource_ref,
      kind: 'mcp',
      operations: definition.operations,
      binding: { handler: entry.controllers?.[0] ?? owner, cursor: '0' },
      log_readers: definition.log_readers,
      revision: 'rev-1',
      event_seq: '0',
      retention_ms: 0,
      data: definition.data,
    };
    try {
      await node.resources.describe(gatewayAuth, definition.resource_ref);
    } catch (error) {
      if (!(error instanceof OnlyneError) || error.error.code !== 'not_found') throw error;
      await node.resources.register(gatewayAuth, resource);
    }
  }

  return {
    node, config, connections,
    async stop(): Promise<void> {
      node.client.stopMaintenance();
      for (const connection of connections.values()) await connection.close();
    },
  };
}
