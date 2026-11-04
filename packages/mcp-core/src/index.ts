import type { Json, NodeId, OperationResult, Resource, ResourceDriver, ResourceRef } from '@onlyne/lib';
import { OnlyneError } from '@onlyne/lib';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StreamableHTTPClientTransport } from '@modelcontextprotocol/sdk/client/streamableHttp.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';
import {
  ErrorCode, McpError,
  ResourceListChangedNotificationSchema,
  ResourceUpdatedNotificationSchema,
  type ListResourcesResult,
  type ReadResourceResult,
  type ServerCapabilities,
} from '@modelcontextprotocol/sdk/types.js';

/** Shared settings of one configured MCP server. */
interface McpServerOptionsBase {
  name: string;
  /** Per-request timeout for tool calls and resource reads; the default matches the library's driver wait. */
  timeoutMs?: number;
  /**
   * The local tool policy (RFC 0015 §4). Absent allows every tool the server lists; an empty
   * list allows none. The policy is checked before any request leaves this node.
   */
  allowedTools?: string[];
}

/** An HTTP streamable MCP server; `url` is the endpoint and `headers` carry its API key. */
export interface McpHttpServerOptions extends McpServerOptionsBase {
  transport?: 'http';
  url: string;
  /** Extra headers per request: Hindsight puts its API key here when enabled. */
  headers?: Record<string, string>;
}

/**
 * A locally spawned MCP server. `command` and `args` are handed to spawn as-is; nothing is
 * joined into a shell string, so a value can never become shell syntax.
 */
export interface McpStdioServerOptions extends McpServerOptionsBase {
  transport: 'stdio';
  command: string;
  args?: string[];
  env?: Record<string, string>;
  cwd?: string;
}

/** One MCP server connection: exactly one of the HTTP and stdio transport contracts. */
export type McpServerOptions = McpHttpServerOptions | McpStdioServerOptions;

/** One entry of `tools/list`, kept as the bridge and the scheduler need it. */
export interface McpToolInfo {
  name: string;
  description?: string;
  annotations?: { readOnlyHint?: boolean };
}

/** One entry of `resources/list`, typed by the official SDK. */
export type McpResourceInfo = ListResourcesResult['resources'][number];

/** One page of `resources/list`; `nextCursor` is the server's own opaque cursor. */
export type McpResourcePage = Pick<ListResourcesResult, 'resources' | 'nextCursor'>;

/** The result of one `resources/read`: data only, never a message or a thread entry. */
export type McpResourceRead = ReadResourceResult;

/** The result of one tool call, classified for the operation's three-way outcome (RFC 0015 §3). */
export type McpOutcome =
  | { kind: 'result'; content?: Json; structuredContent?: Json }
  | { kind: 'rejected'; text: string }
  | { kind: 'unknown'; text: string };

const DEFAULT_TIMEOUT_MS = 30_000;
/** A resources/list loop that never ends is a broken server, not a large catalog. */
const MAX_RESOURCE_PAGES = 1_000;
const wire = (value: object): Json => JSON.parse(JSON.stringify(value)) as Json;
const messageOf = (error: unknown): string => (error instanceof Error ? error.message : String(error));

/**
 * One MCP server connection on the official SDK: `Client` over either the streamable HTTP
 * transport or a spawned stdio child. The SDK owns the session id, the handshake, SSE parsing,
 * reconnect semantics and the stdio process lifecycle; this class is the façade the bridge and
 * the gateway see. The local tool policy lives here, at the choke point every caller (resource
 * operations, scheduled mounts, direct host calls) shares.
 */
export class McpConnection {
  #client: Client | undefined;
  #connecting: Promise<Client> | undefined;
  #closed = false;
  readonly #options: McpServerOptions;
  readonly #allowed: Set<string> | undefined;
  #tools: McpToolInfo[] = [];
  #resources: McpResourceInfo[] = [];
  #subscribed = new Set<string>();
  #resourceUpdatedListeners = new Set<(uri: string) => void>();
  #resourceListChangedListeners = new Set<(resources: ReadonlyArray<McpResourceInfo>, error?: string) => void>();
  #subscriptionLostListeners = new Set<(uri: string, reason: string) => void>();

  constructor(options: McpServerOptions) {
    this.#options = options;
    this.#allowed = options.allowedTools === undefined ? undefined : new Set(options.allowedTools);
  }

  get name(): string { return this.#options.name; }

  get transport(): 'http' | 'stdio' {
    return this.#options.transport === 'stdio' ? 'stdio' : 'http';
  }

  /** The HTTP endpoint; empty for a stdio server, which has no URL. */
  get url(): string { return this.#options.transport === 'stdio' ? '' : this.#options.url; }

  get tools(): McpToolInfo[] { return this.#tools; }

  /** The last `resources/list` result; empty when the server declares no resources capability. */
  get resources(): McpResourceInfo[] { return this.#resources; }

  /** What the server negotiated at `initialize`; undefined before the first successful connect. */
  get capabilities(): ServerCapabilities | undefined {
    return this.#client?.getServerCapabilities();
  }

  get canListResources(): boolean { return this.capabilities?.resources !== undefined; }

  get canSubscribe(): boolean { return this.capabilities?.resources?.subscribe === true; }

  /** The URIs this node currently holds a subscription for. */
  get subscriptions(): ReadonlyArray<string> { return [...this.#subscribed]; }

  /** The local policy decision for one tool name. */
  allows(tool: string): boolean {
    return this.#allowed === undefined || this.#allowed.has(tool);
  }

  /** Connects, reads the tool list and records the negotiated capabilities. */
  async start(): Promise<void> {
    this.#tools = await this.refreshTools();
  }

  async refreshTools(): Promise<McpToolInfo[]> {
    const client = await this.#ensureConnected();
    const result = await client.listTools(undefined, this.#requestOptions());
    this.#tools = result.tools.map(tool => ({
      name: tool.name,
      ...(tool.description === undefined ? {} : { description: tool.description }),
      ...(tool.annotations === undefined ? {} : { annotations: { readOnlyHint: tool.annotations.readOnlyHint } }),
    }));
    return this.#tools;
  }

  /**
   * The catalog entry for one operation. A name the current list already holds answers from the
   * list; any other name refreshes `tools/list` once — connecting first when the session is down
   * — so a catalog a previous run cached cannot turn a reachable tool into
   * `unsupported_delivery`, and a name the live server does not list stays `unsupported_delivery`.
   * The local policy runs before either, so a denied tool never reaches the network (RFC 0015 §4).
   */
  async resolveTool(name: string): Promise<McpToolInfo> {
    if (!this.allows(name)) {
      throw new OnlyneError('forbidden', `The local policy does not allow the tool ${name} on ${this.name}.`);
    }
    if (this.#closed) {
      throw new OnlyneError('unavailable', `The MCP connection ${this.name} is closed.`);
    }
    const known = this.#tools.find(tool => tool.name === name);
    if (known !== undefined) return known;
    try {
      await this.refreshTools();
    } catch (error) {
      if (error instanceof OnlyneError) throw error;
      throw new OnlyneError('unavailable', `The MCP server ${this.name} could not list its tools.`, { source: messageOf(error) });
    }
    const tool = this.#tools.find(candidate => candidate.name === name);
    if (tool === undefined) {
      throw new OnlyneError('unsupported_delivery', `The tool ${name} is not in ${this.name}'s tools/list.`);
    }
    return tool;
  }

  async call(name: string, arguments_: Json): Promise<McpOutcome> {
    // The policy is checked before `#ensureConnected`, so a denied tool never reaches the network
    // (and a scheduled mount cannot bypass the policy by calling the connection directly).
    if (!this.allows(name)) {
      throw new OnlyneError('forbidden', `The local policy does not allow the tool ${name} on ${this.name}.`);
    }
    try {
      const client = await this.#ensureConnected();
      const result = await client.callTool({ name, arguments: arguments_ as Record<string, unknown> },
        undefined, this.#requestOptions());
      if (result.isError === true) {
        return { kind: 'rejected', text: textOf(result.content) };
      }
      return {
        kind: 'result',
        ...(result.content === undefined ? {} : { content: result.content as Json }),
        ...(result.structuredContent === undefined ? {} : { structuredContent: result.structuredContent as Json }),
      };
    } catch (error) {
      if (error instanceof OnlyneError) throw error;
      // A transport failure clears the client handle so the next call attempts a fresh handshake.
      this.#client = undefined;
      return { kind: 'unknown', text: messageOf(error) };
    }
  }

  /** One page of `resources/list`; the cursor is the server's own value. */
  async listResources(cursor?: string): Promise<McpResourcePage> {
    return this.#resourceRequest('list resources', async () => {
      const client = await this.#ensureConnected();
      this.#requireResourceCapability(client);
      const result = await client.listResources(cursor === undefined ? undefined : { cursor }, this.#requestOptions());
      return {
        resources: result.resources,
        ...(result.nextCursor === undefined ? {} : { nextCursor: result.nextCursor }),
      };
    });
  }

  /** The complete catalog, walking `nextCursor` pages. */
  async refreshResources(): Promise<McpResourceInfo[]> {
    const resources: McpResourceInfo[] = [];
    const seen = new Set<string>();
    let cursor: string | undefined;
    for (let page = 0; page < MAX_RESOURCE_PAGES; page++) {
      const result = await this.listResources(cursor);
      resources.push(...result.resources);
      cursor = result.nextCursor;
      if (cursor === undefined) {
        this.#resources = resources;
        return this.#resources;
      }
      if (seen.has(cursor)) {
        throw new OnlyneError('unavailable', `The MCP server ${this.name} repeated a resources page cursor.`);
      }
      seen.add(cursor);
    }
    throw new OnlyneError('unavailable', `The MCP server ${this.name} returned more than ${MAX_RESOURCE_PAGES} resource pages.`);
  }

  /** A read-only `resources/read`; the contents are data and never become a message. */
  async readResource(uri: string): Promise<McpResourceRead> {
    this.#requireUri(uri);
    return this.#resourceRequest(`read ${uri}`, async () => {
      const client = await this.#ensureConnected();
      this.#requireResourceCapability(client);
      return client.readResource({ uri }, this.#requestOptions());
    });
  }

  /** Subscribes this node to one resource's change notifications (RFC 0015 §4: a local action). */
  async subscribe(uri: string): Promise<void> {
    this.#requireUri(uri);
    await this.#resourceRequest(`subscribe to ${uri}`, async () => {
      const client = await this.#ensureConnected();
      this.#requireSubscribeCapability(client);
      if (this.#subscribed.has(uri)) return;
      await client.subscribeResource({ uri }, this.#requestOptions());
      this.#subscribed.add(uri);
    });
  }

  /** Withdraws a subscription; a URI this node never subscribed costs no request. */
  async unsubscribe(uri: string): Promise<void> {
    this.#requireUri(uri);
    const client = this.#client;
    this.#subscribed.delete(uri);
    if (client === undefined) return;
    await this.#resourceRequest(`unsubscribe from ${uri}`,
      () => client.unsubscribeResource({ uri }, this.#requestOptions()));
  }

  /** Withdraws every live subscription, leaving the session open. */
  async unsubscribeAll(): Promise<void> {
    await Promise.all([...this.#subscribed].map(uri => this.unsubscribe(uri).catch(() => undefined)));
  }

  /**
   * Aligns subscriptions with a catalog: subscribes newly listed resources and withdraws the
   * ones that disappeared. A server without `resources.subscribe` simply holds no subscriptions.
   */
  async syncSubscriptions(resources?: ReadonlyArray<McpResourceInfo>): Promise<string[]> {
    const client = await this.#resourceRequest('list resources', () => this.#ensureConnected());
    if (client.getServerCapabilities()?.resources?.subscribe !== true) return [];
    const catalog = resources ?? await this.refreshResources();
    const desired = new Set(catalog.map(resource => resource.uri));
    for (const uri of [...this.#subscribed]) {
      if (!desired.has(uri)) await this.unsubscribe(uri);
    }
    for (const uri of desired) {
      if (!this.#subscribed.has(uri)) await this.subscribe(uri);
    }
    return [...this.#subscribed];
  }

  /** Observes `notifications/resources/updated`; returns the unsubscribe function. */
  onResourceUpdated(listener: (uri: string) => void): () => void {
    this.#resourceUpdatedListeners.add(listener);
    return () => { this.#resourceUpdatedListeners.delete(listener); };
  }

  /** Observes `notifications/resources/list_changed` with the refreshed catalog. */
  onResourceListChanged(listener: (resources: ReadonlyArray<McpResourceInfo>, error?: string) => void): () => void {
    this.#resourceListChangedListeners.add(listener);
    return () => { this.#resourceListChangedListeners.delete(listener); };
  }

  /** Reports a subscription that could not be restored after a reconnect (RFC 0015 §5). */
  onSubscriptionLost(listener: (uri: string, reason: string) => void): () => void {
    this.#subscriptionLostListeners.add(listener);
    return () => { this.#subscriptionLostListeners.delete(listener); };
  }

  async close(): Promise<void> {
    // RFC 0015 §4: closing the bridge withdraws its subscriptions before the session goes.
    const client = this.#client;
    await this.unsubscribeAll();
    this.#closed = true;
    this.#client = undefined;
    this.#subscribed.clear();
    if (client) {
      await client.close().catch(() => undefined);
    }
    this.#resourceUpdatedListeners.clear();
    this.#resourceListChangedListeners.clear();
    this.#subscriptionLostListeners.clear();
  }

  /** Reconnecting re-runs initialize, as RFC 0015 §5 requires; the SDK throws when offline. */
  async #ensureConnected(): Promise<Client> {
    if (this.#closed) {
      throw new OnlyneError('unavailable', `The MCP connection ${this.name} is closed.`);
    }
    if (this.#client !== undefined) return this.#client;
    // Concurrent first calls share one handshake: a second attempt would spawn a second child.
    this.#connecting ??= this.#open();
    const attempt = this.#connecting;
    try {
      return await attempt;
    } finally {
      if (this.#connecting === attempt) this.#connecting = undefined;
    }
  }

  async #open(): Promise<Client> {
    const client = new Client({ name: 'onlyne-mcp', version: '3.0.0' }, { capabilities: {} });
    await client.connect(this.#transport(), this.#requestOptions());
    if (this.#closed) {
      // `close()` ran during the handshake and had no client to close, so this one must not leak.
      await client.close().catch(() => undefined);
      throw new OnlyneError('unavailable', `The MCP connection ${this.name} is closed.`);
    }
    this.#client = client;
    this.#installNotificationHandlers(client);
    await this.#restoreSubscriptions(client);
    return client;
  }

  #transport(): StreamableHTTPClientTransport | StdioClientTransport {
    const options = this.#options;
    if (options.transport === 'stdio') {
      // The SDK spawns `command` with `args` directly (`shell: false`); no string is interpolated.
      return new StdioClientTransport({
        command: options.command,
        ...(options.args === undefined ? {} : { args: [...options.args] }),
        ...(options.env === undefined ? {} : { env: { ...options.env } }),
        ...(options.cwd === undefined ? {} : { cwd: options.cwd }),
      });
    }
    return new StreamableHTTPClientTransport(new URL(options.url), {
      requestInit: { headers: options.headers },
    });
  }

  #installNotificationHandlers(client: Client): void {
    client.setNotificationHandler(ResourceUpdatedNotificationSchema, notification => {
      // The notification only reports the URI; the body is a later `resources/read` (RFC 0015 §2).
      for (const listener of [...this.#resourceUpdatedListeners]) listener(notification.params.uri);
    });
    client.setNotificationHandler(ResourceListChangedNotificationSchema, () => {
      void this.refreshResources().then(
        resources => {
          for (const listener of [...this.#resourceListChangedListeners]) listener(resources);
        },
        error => {
          for (const listener of [...this.#resourceListChangedListeners]) listener(this.#resources, messageOf(error));
        },
      );
    });
  }

  /** RFC 0015 §5: a reconnect must re-subscribe; a failed restore is reported, not faked. */
  async #restoreSubscriptions(client: Client): Promise<void> {
    const pending = [...this.#subscribed];
    if (pending.length === 0) return;
    if (client.getServerCapabilities()?.resources?.subscribe !== true) {
      for (const uri of pending) {
        this.#subscribed.delete(uri);
        this.#emitSubscriptionLost(uri, 'The server no longer declares the resources.subscribe capability.');
      }
      return;
    }
    for (const uri of pending) {
      try {
        await client.subscribeResource({ uri }, this.#requestOptions());
      } catch (error) {
        this.#subscribed.delete(uri);
        this.#emitSubscriptionLost(uri, messageOf(error));
      }
    }
  }

  #emitSubscriptionLost(uri: string, reason: string): void {
    for (const listener of [...this.#subscriptionLostListeners]) listener(uri, reason);
  }

  #requestOptions(): { resetTimeoutOnProgress: boolean; timeout: number } {
    return { resetTimeoutOnProgress: false, timeout: this.#options.timeoutMs ?? DEFAULT_TIMEOUT_MS };
  }

  #requireUri(uri: string): void {
    if (typeof uri !== 'string' || uri.length === 0) {
      throw new OnlyneError('invalid_request', 'A resource URI is a non-empty string.');
    }
  }

  #requireResourceCapability(client: Client): void {
    if (client.getServerCapabilities()?.resources === undefined) {
      throw new OnlyneError('unsupported_delivery', `The MCP server ${this.name} declares no resources capability.`);
    }
  }

  #requireSubscribeCapability(client: Client): void {
    if (client.getServerCapabilities()?.resources?.subscribe !== true) {
      throw new OnlyneError('unsupported_delivery', `The MCP server ${this.name} does not declare resources.subscribe.`);
    }
  }

  /** Maps SDK and transport failures for the resource primitives, never a bare string. */
  async #resourceRequest<T>(action: string, run: () => Promise<T>): Promise<T> {
    try {
      return await run();
    } catch (error) {
      if (error instanceof OnlyneError) throw error;
      if (error instanceof McpError) {
        if (error.code === ErrorCode.MethodNotFound) {
          throw new OnlyneError('unsupported_delivery', `The MCP server ${this.name} does not support ${action}.`, { source: error.message });
        }
        if (error.code === ErrorCode.InvalidParams || error.code === ErrorCode.InvalidRequest) {
          throw new OnlyneError('invalid_request', `The MCP server ${this.name} rejected ${action}.`, { source: error.message });
        }
        if (error.code === ErrorCode.ConnectionClosed) this.#client = undefined;
        throw new OnlyneError('unavailable', `The MCP server ${this.name} could not ${action}.`, { source: error.message, code: error.code });
      }
      this.#client = undefined;
      throw new OnlyneError('unavailable', `The MCP transport failed while trying to ${action}.`, { source: messageOf(error) });
    }
  }
}

/** The resource clause one MCP server contributes: its id, operations and diagnostic data. */
export interface McpResourceDef {
  resource_ref: ResourceRef;
  operations: string[];
  binding: { handler: NodeId; cursor: string };
  log_readers: NodeId[];
  data: Json;
}

/** Extra server facts a registration can carry; both are optional for a down server. */
export interface McpResourceOptions {
  /** The `resources/list` catalog; absent when the server is unreachable or declares none. */
  resources?: ReadonlyArray<McpResourceInfo>;
  /** The capabilities negotiated at `initialize`, when the session is up. */
  capabilities?: ServerCapabilities;
}

/** One line of a diagnostic description, bounded so a chatty server cannot bloat the data. */
function summaryOf(text: string | undefined, limit = 160): string {
  if (text === undefined) return '';
  const line = text.replace(/\s+/g, ' ').trim();
  return line.length <= limit ? line : `${line.slice(0, limit - 1)}…`;
}

/**
 * Builds the Onlyne resource a server maps to (RFC 0015 §2: one server, one tool set). The
 * `data` field is the human- and machine-readable description: concrete tool names and resource
 * URIs, the transport, the negotiated `features` (capabilities) and the local tool policy.
 */
export function resourceOf(
  owner: string,
  server: McpServerOptions,
  tools: ReadonlyArray<McpToolInfo>,
  controllers: string[],
  options: McpResourceOptions = {},
): McpResourceDef {
  const resources = options.resources ?? [];
  const capabilities = options.capabilities;
  const location = server.transport === 'stdio'
    ? {
      transport: 'stdio' as const,
      command: server.command,
      ...(server.args === undefined || server.args.length === 0 ? {} : { args: server.args }),
      ...(server.cwd === undefined ? {} : { cwd: server.cwd }),
    }
    : { transport: 'http' as const, url: server.url };
  const transport = location.transport;
  const toolLine = tools.length === 0
    ? 'no tools'
    : `tools ${tools.map(tool => {
      const summary = summaryOf(tool.description);
      return summary.length === 0 ? tool.name : `${tool.name} (${summary})`;
    }).join(', ')}`;
  const resourceLine = resources.length === 0
    ? 'no resources'
    : `resources ${resources.map(resource => {
      const summary = summaryOf(resource.description);
      return summary.length === 0 ? resource.uri : `${resource.uri} (${summary})`;
    }).join(', ')}`;
  return {
    resource_ref: { owner, id: server.name },
    operations: tools.map(tool => tool.name),
    binding: { handler: owner, cursor: '0' },
    log_readers: controllers.length > 0 ? controllers : [owner],
    data: wire({
      kind: 'mcp_server',
      ...location,
      ...(server.allowedTools === undefined ? {} : { allowed_tools: server.allowedTools }),
      tools: tools.map(tool => ({
        name: tool.name,
        ...(typeof tool.description === 'string' ? { description: summaryOf(tool.description, 200) } : {}),
        ...(tool.annotations?.readOnlyHint === true ? { read_only: true } : {}),
      })),
      resources: resources.map(resource => ({
        uri: resource.uri,
        name: resource.name,
        ...(typeof resource.description === 'string' ? { description: summaryOf(resource.description, 200) } : {}),
        ...(typeof resource.mimeType === 'string' ? { mime_type: resource.mimeType } : {}),
      })),
      description: `MCP server ${server.name} over ${transport}: ${toolLine}; ${resourceLine}.`,
      ...(capabilities === undefined ? {} : {
        features: {
          tools: capabilities.tools !== undefined,
          resources: capabilities.resources !== undefined,
          resource_subscribe: capabilities.resources?.subscribe === true,
          resource_list_changed: capabilities.resources?.listChanged === true,
        },
      }),
      ...(controllers.length > 0 ? {} : { note: 'owner only' }),
    }),
  };
}

/** Host callbacks for the resource primitives; all are observations, never business writes. */
export interface McpBridgeOptions {
  onResourceUpdated?(server: string, uri: string): void;
  onResourceListChanged?(server: string, resources: ReadonlyArray<McpResourceInfo>, error?: string): void;
  onSubscriptionLost?(server: string, uri: string, reason: string): void;
}

/**
 * The bridge from resource operations to `tools/call`, plus the read-only MCP resource
 * primitives. `validate` asks the connection for the operation's catalog entry: a name the live
 * session does not list refreshes `tools/list` once, so a catalog a previous run cached stays
 * callable while a name the server really dropped never reaches `tools/call` (RFC 0015 §2, §4).
 */
export interface McpBridge extends ResourceDriver {
  listResources(server: string, cursor?: string): Promise<McpResourcePage>;
  readResource(server: string, uri: string): Promise<Json>;
  subscribe(server: string, uri: string): Promise<void>;
  unsubscribe(server: string, uri: string): Promise<void>;
  /** Subscribes every listed resource and withdraws vanished ones; returns the live set. */
  syncSubscriptions(server: string): Promise<string[]>;
  /** Withdraws every subscription this bridge made. */
  close(): Promise<void>;
}

export function createMcpBridge(
  servers: Array<{ server: McpServerOptions; connection: McpConnection }>,
  options: McpBridgeOptions = {},
): McpBridge {
  const byServer = new Map(servers.map(entry => [entry.server.name, entry]));
  const disposers: Array<() => void> = [];
  let closed = false;

  for (const entry of servers) {
    const name = entry.server.name;
    disposers.push(entry.connection.onResourceUpdated(uri => {
      // A change notification is data: it never becomes an agent message or a thread entry.
      options.onResourceUpdated?.(name, uri);
    }));
    disposers.push(entry.connection.onResourceListChanged((resources, error) => {
      void entry.connection.syncSubscriptions(resources).then(
        () => { options.onResourceListChanged?.(name, resources, error); },
        syncError => {
          options.onResourceListChanged?.(name, resources, error ?? messageOf(syncError));
        },
      );
    }));
    disposers.push(entry.connection.onSubscriptionLost((uri, reason) => {
      options.onSubscriptionLost?.(name, uri, reason);
    }));
  }

  function requireServer(name: string): { server: McpServerOptions; connection: McpConnection } {
    if (closed) throw new OnlyneError('unavailable', 'The MCP bridge is closed.');
    const entry = byServer.get(name);
    if (entry === undefined) {
      throw new OnlyneError('invalid_request', `No MCP server serves the resource ${name}.`);
    }
    return entry;
  }

  async function requireTool(ref: ResourceRef, operation: string): Promise<{
    entry: { server: McpServerOptions; connection: McpConnection };
    tool: McpToolInfo;
  }> {
    const entry = requireServer(ref.id);
    // `resolveTool` checks the policy before any request (a denied tool is `forbidden`, not
    // `unsupported`) and refreshes the catalog once when the name is not in the pinned list.
    const tool = await entry.connection.resolveTool(operation);
    return { entry, tool };
  }

  return {
    async validate(resource: Resource, operation: string, data: Json | undefined): Promise<void> {
      await requireTool(resource.resource_ref, operation);
      // `arguments` for `tools/call` is an object when present (RFC 0015 §2).
      if (data !== undefined && (data === null || typeof data !== 'object' || Array.isArray(data))) {
        throw new OnlyneError('invalid_request', `The arguments for ${operation} are an object.`);
      }
    },

    async execute(resource: Resource, request): Promise<Omit<OperationResult, 'operation_id'>> {
      const { entry, tool } = await requireTool(resource.resource_ref, request.operation);
      const arguments_ = (request.data ?? {}) as Json;
      const outcome = await entry.connection.call(request.operation, arguments_);
      if (outcome.kind === 'unknown') {
        // The transport never gave an answer: the call may or may not have run (RFC 0015 §3).
        return { state: 'unknown', error: { code: 'unavailable', message: 'The MCP server gave no answer.', retryable: true, details: { source: outcome.text } } };
      }
      if (outcome.kind === 'rejected') {
        // A read-only tool cannot have side effects, so its failure is a settled abort; a
        // side-effecting tool's failure proves nothing about what it already did.
        if (tool.annotations?.readOnlyHint === true) {
          return { state: 'aborted', error: { code: 'unavailable', message: 'The tool reported failure.', retryable: false, details: { source: outcome.text } } };
        }
        return { state: 'unknown', error: { code: 'unavailable', message: 'The tool reported failure; the side effect is unconfirmed.', retryable: true, details: { source: outcome.text } } };
      }
      return {
        state: 'committed',
        data: wire({
          ...(outcome.content === undefined ? {} : { content: outcome.content }),
          ...(outcome.structuredContent === undefined ? {} : { structuredContent: outcome.structuredContent }),
        }),
      };
    },

    async listResources(server: string, cursor?: string): Promise<McpResourcePage> {
      return requireServer(server).connection.listResources(cursor);
    },

    async readResource(server: string, uri: string): Promise<Json> {
      // Data only: no message admission, no thread entry, no business-state write.
      return wire(await requireServer(server).connection.readResource(uri));
    },

    async subscribe(server: string, uri: string): Promise<void> {
      await requireServer(server).connection.subscribe(uri);
    },

    async unsubscribe(server: string, uri: string): Promise<void> {
      await requireServer(server).connection.unsubscribe(uri);
    },

    async syncSubscriptions(server: string): Promise<string[]> {
      return requireServer(server).connection.syncSubscriptions();
    },

    async close(): Promise<void> {
      if (closed) return;
      closed = true;
      await Promise.all(servers.map(entry => entry.connection.unsubscribeAll()));
      for (const dispose of disposers) dispose();
      disposers.length = 0;
    },
  };
}

/** An error result's content is text blocks; their `text` fields are the readable message. */
function textOf(content: unknown): string {
  if (typeof content === 'string') return content;
  if (Array.isArray(content)) {
    return content
      .map(block => {
        if (typeof block !== 'object' || block === null || !('text' in block)) return '';
        return typeof block.text === 'string' ? block.text : '';
      })
      .filter(text => text.length > 0)
      .join('\n');
  }
  return JSON.stringify(content ?? '');
}
