import type { Json, NodeId, Resource, ResourceDriver, ResourceRef } from '@onlyne/lib';
import { OnlyneError } from '@onlyne/lib';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StreamableHTTPClientTransport } from '@modelcontextprotocol/sdk/client/streamablehttp.js';

export interface McpServerOptions {
  name: string;
  url: string;
  /** Extra headers per request: Hindsight puts its API key here when enabled. */
  headers?: Record<string, string>;
  /** Per-request timeout for tool calls; the default matches the library's driver wait. */
  timeoutMs?: number;
}

/** The result of one tool call, classified for the operation's three-way outcome (RFC 0015 §3). */
export type McpOutcome =
  | { kind: 'result'; content?: Json; structuredContent?: Json }
  | { kind: 'rejected'; text: string }
  | { kind: 'unknown'; text: string };

const DEFAULT_TIMEOUT_MS = 30_000;
const wire = (value: object): Json => JSON.parse(JSON.stringify(value)) as Json;

/**
 * One MCP server connection on the official SDK: `Client` over the streamable HTTP transport.
 * The SDK owns the session id, the handshake, SSE parsing and the reconnect semantics; this
 * class is only the façade the bridge and the gateway see.
 */
export class McpConnection {
  #client: Client | undefined;
  readonly #options: McpServerOptions;
  #tools: Array<{ name: string; description?: string; annotations?: { readOnlyHint?: boolean } }> = [];

  constructor(options: McpServerOptions) {
    this.#options = options;
  }

  get url(): string { return this.#options.url; }

  get tools(): Array<{ name: string; description?: string; annotations?: { readOnlyHint?: boolean } }> { return this.#tools; }

  /** Connects and reads the tool list; the SDK handles initialize and the session handshake. */
  async start(): Promise<void> {
    this.#tools = await this.refreshTools();
  }

  async refreshTools(): Promise<Array<{ name: string; description?: string; annotations?: { readOnlyHint?: boolean } }>> {
    const client = await this.#ensureConnected();
    const result = await client.listTools();
    this.#tools = result.tools.map(tool => ({
      name: tool.name,
      ...(tool.description === undefined ? {} : { description: tool.description }),
      ...(tool.annotations === undefined ? {} : { annotations: { readOnlyHint: tool.annotations.readOnlyHint } }),
    }));
    return this.#tools;
  }

  async call(name: string, arguments_: Json): Promise<McpOutcome> {
    try {
      const client = await this.#ensureConnected();
      const result = await client.callTool({ name, arguments: arguments_ as Record<string, unknown> },
        undefined, { resetTimeoutOnProgress: false, timeout: this.#options.timeoutMs ?? DEFAULT_TIMEOUT_MS });
      if (result.isError === true) {
        return { kind: 'rejected', text: textOf(result.content) };
      }
      return {
        kind: 'result',
        ...(result.content === undefined ? {} : { content: result.content as Json }),
        ...(result.structuredContent === undefined ? {} : { structuredContent: result.structuredContent as Json }),
      };
    } catch (error) {
      // A transport failure clears the client handle so the next call attempts a fresh handshake.
      this.#client = undefined;
      return { kind: 'unknown', text: error instanceof Error ? error.message : String(error) };
    }
  }

  async close(): Promise<void> {
    const client = this.#client;
    this.#client = undefined;
    if (client) await client.close().catch(() => undefined);
  }

  /** Reconnecting re-runs initialize, as RFC 0015 §5 requires; the SDK throws when offline. */
  async #ensureConnected(): Promise<Client> {
    if (this.#client !== undefined) return this.#client;
    const transport = new StreamableHTTPClientTransport(new URL(this.#options.url), {
      requestInit: { headers: this.#options.headers },
    });
    const client = new Client({ name: 'onlyne-mcp', version: '3.0.0' }, { capabilities: {} });
    await client.connect(transport);
    this.#client = client;
    return client;
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

/** Builds the Onlyne resource a server maps to (RFC 0015 §2: one server, one tool set). */
export function resourceOf(
  owner: string,
  server: McpServerOptions,
  tools: ReadonlyArray<{ name: string; description?: string }>,
  controllers: string[],
): McpResourceDef {
  return {
    resource_ref: { owner, id: server.name },
    operations: tools.map(tool => tool.name),
    binding: { handler: owner, cursor: '0' },
    log_readers: controllers.length > 0 ? controllers : [owner],
    data: wire({
      kind: 'mcp_server',
      url: server.url,
      tools: tools.map(tool => ({
        name: tool.name,
        ...(typeof tool.description === 'string' ? { description: tool.description.slice(0, 200) } : {}),
      })),
      ...(controllers.length > 0 ? {} : { note: 'owner only' }),
    }),
  };
}

/**
 * The bridge from resource operations to `tools/call`. The tool list is pinned per server, so
 * `validate` answers from the cache and a request for an unknown tool never leaves the node
 * (RFC 0015 §2: `unsupported_delivery`, not sent).
 */
export function createMcpBridge(servers: Array<{ server: McpServerOptions; connection: McpConnection }>): ResourceDriver {
  const byServer = new Map(servers.map(entry => [entry.server.name, entry]));

  function requireTool(ref: ResourceRef, operation: string): { entry: { server: McpServerOptions; connection: McpConnection }; tool: { name: string; annotations?: { readOnlyHint?: boolean } } } {
    const entry = byServer.get(ref.id);
    if (entry === undefined) {
      throw new OnlyneError('invalid_request', `No MCP server serves the resource ${ref.id}.`);
    }
    const tool = entry.connection.tools.find(candidate => candidate.name === operation);
    if (tool === undefined) {
      throw new OnlyneError('unsupported_delivery', `The tool ${operation} is not in ${ref.id}'s tools/list.`);
    }
    return { entry, tool };
  }

  return {
    async validate(resource: Resource, operation: string, data: Json | undefined): Promise<void> {
      requireTool(resource.resource_ref, operation);
      // `arguments` for `tools/call` is an object when present (RFC 0015 §2).
      if (data !== undefined && (data === null || typeof data !== 'object' || Array.isArray(data))) {
        throw new OnlyneError('invalid_request', `The arguments for ${operation} are an object.`);
      }
    },

    async execute(resource: Resource, request): Promise<Omit<import('@onlyne/lib').OperationResult, 'operation_id'>> {
      const { entry, tool } = requireTool(resource.resource_ref, request.operation);
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
  };
}

/** An error result's content is text blocks; their `text` fields are the readable message. */
function textOf(content: unknown): string {
  if (typeof content === 'string') return content;
  if (Array.isArray(content)) {
    return content
      .map(block => (typeof block === 'object' && block !== null && 'text' in block && typeof (block as { text?: unknown }).text === 'string'
        ? (block as { text: string }).text
        : ''))
      .filter(text => text.length > 0)
      .join('\n');
  }
  return JSON.stringify(content ?? '');
}
