import {
  OnlyneNode, OnlyneError, type Auth, type KeyHook, type MaintenanceClock, type NodeId, type Plugins,
  type Resource, type ResourceDriver,
} from '@onlyne/lib';
import {
  createMcpBridge, resourceOf, McpConnection,
  type McpBridge, type McpResourceInfo, type McpServerOptions, type McpToolInfo,
} from '@onlyne/mcp-core';
import type { GatewayConfig, ServerConfig } from './config.js';
import { FileStorage } from '@onlyne/im-gateway/storage';
import { createMountService, MOUNTS_RESOURCE_PREFIX, type MountService, type MountTransport } from './mounts.js';
import { t } from './gateway-text.js';

export type { GatewayConfig, ServerConfig } from './config.js';
export type { McpBridge, McpResourceInfo, McpResourcePage } from '@onlyne/mcp-core';
export type { MountService } from './mounts.js';

export interface McpGateway {
  readonly node: OnlyneNode;
  readonly config: GatewayConfig;
  /** The MCP connections, one per configured server, keyed by resource id. */
  readonly connections: Map<string, McpConnection>;
  /** The resource driver plus the read-only MCP resource primitives. */
  readonly bridge: McpBridge;
  /** The scheduled-mount service; its driver is part of the node's resource routing. */
  readonly mounts: MountService;
  /** Display lines for the node host's TUI: servers, mounts, push queue, recent results. */
  status(): Promise<string[]>;
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
 * Assembles the gateway: one session per configured MCP server, one Onlyne resource per server,
 * and — when configured — one scheduler resource per mount controller. A server that cannot be
 * reached at startup still registers (with its last known tools if the store has them, an empty
 * list otherwise): the node stays up, and the session re-initializes and refreshes `tools/list`
 * on the first operation that needs the catalog (RFC 0015 §5).
 */
export interface McpGatewayOptions {
  log?: (message: string, fields?: Record<string, unknown>) => void;
  /**
   * RFC 0017 identity key. The CLI always supplies one; a programmatic caller may omit it and
   * keep the keyless loopback binding the tests use.
   */
  keys?: KeyHook;
}

/** Maps the configuration's transport union onto the core connection contract. */
function serverOptionsOf(entry: ServerConfig): McpServerOptions {
  const shared = {
    name: entry.name,
    ...(entry.timeout_ms === undefined ? {} : { timeoutMs: entry.timeout_ms }),
    ...(entry.allowed_tools === undefined ? {} : { allowedTools: entry.allowed_tools }),
  };
  if (entry.transport === 'stdio') {
    return {
      ...shared,
      transport: 'stdio',
      command: entry.command,
      ...(entry.args === undefined ? {} : { args: entry.args }),
      ...(entry.env === undefined ? {} : { env: entry.env }),
      ...(entry.cwd === undefined ? {} : { cwd: entry.cwd }),
    };
  }
  return {
    ...shared,
    transport: 'http',
    url: entry.url,
    ...(entry.headers === undefined ? {} : { headers: entry.headers }),
  };
}

export async function createMcpGateway(config: GatewayConfig, options: McpGatewayOptions = {}): Promise<McpGateway> {
  const owner: NodeId = config.node.id;
  const storage = new FileStorage(config.storage_path);
  const connections = new Map<string, McpConnection>();
  const serverEntries: Array<{ server: McpServerOptions; connection: McpConnection }> = [];
  const resourcesByName = new Map<string, McpResourceInfo[]>();
  const toolsByName = new Map<string, McpToolInfo[]>();

  for (const entry of config.servers) {
    const server = serverOptionsOf(entry);
    const connection = new McpConnection(server);
    let tools: Awaited<ReturnType<McpConnection['refreshTools']>> = [];
    let resources: McpResourceInfo[] = [];
    // The last known tool list rides in the store, so a temporarily unreachable server still
    // presents its operations instead of an empty resource.
    const toolsKey = `mcp/tools/${encodeURIComponent(entry.name)}`;
    const resourcesKey = `mcp/resources/${encodeURIComponent(entry.name)}`;
    const cached = await storage.transaction(async tx => tx.get<{ tools: typeof tools }>(toolsKey));
    const cachedResources = await storage.transaction(async tx => tx.get<{ resources: McpResourceInfo[] }>(resourcesKey));
    let started = false;
    try {
      await connection.start();
      started = true;
      tools = connection.tools;
      await storage.transaction(async tx => { await tx.put(toolsKey, { tools }); });
    } catch (err) {
      // Registration proceeds with the cache; the connection connects and refreshes tools/list
      // on the first operation that needs the catalog.
      options.log?.('mcp.server.start.failed', { server: entry.name, error: String(err) });
      tools = cached?.tools ?? [];
    }
    if (started) {
      if (connection.canListResources) {
        try {
          resources = await connection.refreshResources();
          await storage.transaction(async tx => { await tx.put(resourcesKey, { resources }); });
        } catch (err) {
          options.log?.('mcp.server.resources.failed', { server: entry.name, error: String(err) });
          resources = cachedResources?.resources ?? [];
        }
      } else {
        // The server answered and declares no resources capability: an empty catalog is the truth.
        await storage.transaction(async tx => { await tx.put(resourcesKey, { resources: [] }); });
      }
    } else {
      resources = cachedResources?.resources ?? [];
    }
    resourcesByName.set(entry.name, resources);
    toolsByName.set(entry.name, tools);
    connections.set(entry.name, connection);
    serverEntries.push({ server, connection });
  }

  const bridge = createMcpBridge(serverEntries, {
    onResourceUpdated: (server, uri) => options.log?.('mcp.resource.updated', { server, uri }),
    onResourceListChanged: (server, resources, error) => {
      options.log?.('mcp.resource.list.changed', {
        server, count: resources.length,
        ...(error === undefined ? {} : { error }),
      });
      void cacheCatalog(server, resources);
    },
    onSubscriptionLost: (server, uri, reason) => options.log?.('mcp.resource.subscription.lost', { server, uri, reason }),
  });
  // The refreshed catalog rides in the store for the next start, when the server may be down.
  async function cacheCatalog(server: string, resources: ReadonlyArray<McpResourceInfo>): Promise<void> {
    try {
      await storage.transaction(async tx => {
        await tx.put(`mcp/resources/${encodeURIComponent(server)}`, { resources });
      });
    } catch (error) {
      options.log?.('mcp.resources.cache.failed', { server, error: String(error) });
    }
  }
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
      // The gateway stores messages but holds no model context: it can fulfill every plan
      // except an await, which would need a model to answer. Its own scheduled result
      // messages are admitted locally under the thread it owns, so this gate must allow
      // `response: none`; refusing it would turn every run result into a delivery refusal.
      supports: async plan => plan.response !== 'await',
      inject: async () => { throw new OnlyneError('unsupported_delivery', t('error.noContext')); },
    },
    domainCandidates: async domain_id => node.discovery.candidates(domain_id),
    ...(keys === undefined ? {} : { keys }),
  };
  let node: OnlyneNode;
  let mounts: MountService | undefined;
  const requireMounts = (): MountService => {
    if (mounts === undefined) throw new OnlyneError('unsupported_delivery', 'The scheduler is not assembled yet.');
    return mounts;
  };
  // One driver routes a resource by id: the scheduler owns `mounts:<controller>`, every other id
  // belongs to the configured MCP server of that name (config rejects a server on the prefix).
  const resources: ResourceDriver = {
    validate: (resource, operation, data) => (resource.resource_ref.id.startsWith(MOUNTS_RESOURCE_PREFIX)
      ? requireMounts().driver.validate(resource, operation, data)
      : bridge.validate(resource, operation, data)),
    execute: (resource, request, signal) => (resource.resource_ref.id.startsWith(MOUNTS_RESOURCE_PREFIX)
      ? requireMounts().driver.execute(resource, request, signal)
      : bridge.execute(resource, request, signal)),
  };
  node = new OnlyneNode({
    node_id: owner,
    base_url,
    // RFC 0017 §1: a keyed node advertises the scheme it authenticates with. The keyless
    // binding stays for programmatic callers on loopback.
    authentication: keys === undefined ? ['none'] : ['signature'],
    plugins: { ...plugins, storage, resources },
    now: () => Date.now(),
    id: () => crypto.randomUUID(),
  });

  const transport: MountTransport = {
    // One thread per task: the first mount creates it, a restart recovers the same id, and the
    // result messages land in one readable conversation instead of one thread per run.
    ensureThread: async (threadId, title) => {
      try {
        await node.threads.create(node.auth, {
          id: threadId, title,
          idempotency_key: crypto.randomUUID(),
          retry_until: new Date(Date.now() + 86_400_000).toISOString(),
        });
      } catch (error) {
        if (!(error instanceof OnlyneError) || error.error.code !== 'id_conflict') throw error;
      }
    },
    // A target this node cannot name yet is a queue condition, not a peer refusal: the run's
    // result stays pending until discovery finds the controller.
    resolveTarget: async id => { await node.discovery.resolve(id); },
    send: (controller, message) => node.client.send(controller, message, { response: 'none', execution: 'existing' }),
  };
  mounts = createMountService({
    owner, config: config.mounts, servers: config.servers, storage,
    connections, transport,
    ...(options.log === undefined ? {} : { log: options.log }),
  });
  await mounts.start();
  const mountService = mounts;

  const gatewayAuth: Auth = { actor: owner };
  for (const [index, entry] of config.servers.entries()) {
    const controllers = Array.from(new Set([owner, ...(entry.controllers ?? [])]));
    const connection = serverEntries[index]!.connection;
    const capabilities = connection.capabilities;
    // The effective catalog: live tools when the start succeeded, the cached ones when it did
    // not, so a down server still advertises the operations a later call can refresh.
    const definition = resourceOf(owner, serverEntries[index]!.server, toolsByName.get(entry.name) ?? [], controllers, {
      resources: resourcesByName.get(entry.name) ?? [],
      ...(capabilities === undefined ? {} : { capabilities }),
    });
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
    await registerResource(node, gatewayAuth, resource);
  }
  for (const { resource } of mountService.resources()) {
    await registerResource(node, gatewayAuth, resource);
  }
  // The node is the resource endpoint, so it subscribes locally to every listed resource when
  // the server declares `resources.subscribe` (RFC 0015 §4). A server without that capability
  // holds no subscriptions; a failure here is logged and does not take the gateway down.
  for (const entry of serverEntries) {
    try {
      await bridge.syncSubscriptions(entry.server.name);
    } catch (error) {
      options.log?.('mcp.resources.subscribe.failed', { server: entry.server.name, error: String(error) });
    }
  }

  return {
    node, config, connections, bridge, mounts: mountService,
    status: () => mountService.status(),
    async stop(): Promise<void> {
      await mountService.stop();
      node.client.stopMaintenance();
      await bridge.close();
      for (const connection of connections.values()) await connection.close();
    },
  };
}

/** Registration is idempotent for the same definition; a changed definition is a real conflict. */
async function registerResource(node: OnlyneNode, auth: Auth, resource: Resource): Promise<void> {
  try {
    await node.resources.describe(auth, resource.resource_ref);
  } catch (error) {
    if (!(error instanceof OnlyneError) || error.error.code !== 'not_found') throw error;
    await node.resources.register(auth, resource);
  }
}
