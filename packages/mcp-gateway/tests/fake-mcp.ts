import type { Json } from '@onlyne/lib';

/**
 * A scripted MCP server: one Bun.serve speaking the HTTP streamable transport. It hands out a
 * fixed tool list and records every `tools/call` so a test can prove what left the node.
 */
export class FakeMcpServer {
  readonly url: string;
  readonly tools = [
    { name: 'echo', description: 'echo input', inputSchema: { type: 'object' }, annotations: { readOnlyHint: true } },
    { name: 'write_data', description: 'write', inputSchema: { type: 'object' } },
  ];
  readonly calls: Array<{ name: string; arguments: Json }> = [];
  #server: { stop: () => Promise<void> };

  constructor() {
    const handler = async (request: Request): Promise<Response> => {
      if (request.method === 'GET') return new Response(null, { status: 405 });
      const body = JSON.parse(await request.text()) as { method: string; id?: string | number; params?: unknown };
      const respond = (payload: unknown, sessionId?: string): Response => new Response(
        `event: message\ndata: ${JSON.stringify(payload)}\n\n`,
        { headers: { 'content-type': 'text/event-stream', ...(sessionId === undefined ? {} : { 'mcp-session-id': sessionId }) } },
      );
      if (body.method === 'initialize') {
        return respond({ jsonrpc: '2.0', id: body.id, result: { protocolVersion: '2025-03-26', capabilities: { tools: {} }, serverInfo: { name: 'fake', version: '0' } } }, 'sess-1');
      }
      if (body.method === 'notifications/initialized') return new Response(null, { status: 202 });
      if (body.method === 'ping') return respond({ jsonrpc: '2.0', id: body.id, result: {} });
      if (body.method === 'tools/list') {
        return respond({ jsonrpc: '2.0', id: body.id, result: { tools: this.tools } });
      }
      if (body.method === 'tools/call') {
        const params = body.params as { name: string; arguments?: Json };
        this.calls.push({ name: params.name, arguments: params.arguments ?? {} });
        return respond({ jsonrpc: '2.0', id: body.id, result: { content: [{ type: 'text', text: 'ok' }], structuredContent: params.arguments as object } });
      }
      return respond({ jsonrpc: '2.0', id: body.id, error: { code: -32601, message: 'unknown method' } });
    };
    const server = Bun.serve({ port: 0, hostname: '127.0.0.1', fetch: handler });
    this.#server = server;
    this.url = `http://127.0.0.1:${server.port}/mcp`;
  }

  stop(): Promise<void> { return this.#server.stop(); }
}
