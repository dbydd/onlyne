import { OnlyneError, OnlyneNode, type DeliveryPlan, type Json, type Message, type MessageRef, type ThreadRef } from '@onlyne/lib';
import { Server } from '@modelcontextprotocol/sdk/server/index.js';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import {
  CallToolRequestSchema,
  ListResourcesRequestSchema,
  ListToolsRequestSchema,
  ReadResourceRequestSchema,
  type CallToolResult,
} from '@modelcontextprotocol/sdk/types.js';
import { t } from './server-text.ts';

const PAGE = {
  type: 'object',
  properties: {
    page_size: { type: 'integer', minimum: 1, maximum: 100, description: 'Page size, default 50.' },
    cursor: { type: 'string', description: 'Opaque cursor from a previous page.' },
  },
} as const;

const THREAD_REF = {
  owner: { type: 'string', description: 'Thread owner node id.' },
  id: { type: 'string', description: 'Thread id.' },
} as const;

/** The focused tool set: send, read, and inspect what this Onlyne node already holds. */
export const TOOLS = [
  {
    name: 'send_message',
    description: 'Send a message from this Onlyne node to another node. Uses response:none unless you pass async.',
    inputSchema: {
      type: 'object',
      required: ['to', 'msg'],
      properties: {
        to: { type: 'string', description: 'Target node id.' },
        msg: { type: 'string', description: 'Message body.' },
        delivery_kind: { type: 'string', description: 'Delivery kind, default "message".' },
        thread_owner: { type: 'string', description: 'Existing thread owner, with thread_id.' },
        thread_id: { type: 'string', description: 'Existing thread id, with thread_owner.' },
        in_reply_to_sender: { type: 'string', description: 'Original message sender, with in_reply_to_id.' },
        in_reply_to_id: { type: 'string', description: 'Original message id, with in_reply_to_sender.' },
        response: { type: 'string', enum: ['none', 'async'], description: 'Delivery response plan. Default none.' },
        execution: { type: 'string', enum: ['existing', 'new'], description: 'Delivery execution plan. Default existing.' },
      },
    },
  },
  {
    name: 'list_inbox',
    description: 'List this node\'s local inbox (attention records this node holds).',
    inputSchema: PAGE,
  },
  {
    name: 'read_message',
    description: 'Read one message this node holds, by sender and message id.',
    inputSchema: {
      type: 'object',
      required: ['sender', 'id'],
      properties: {
        sender: { type: 'string', description: 'Message sender node id.' },
        id: { type: 'string', description: 'Message id.' },
      },
    },
  },
  {
    name: 'list_threads',
    description: 'List threads this node owns.',
    inputSchema: PAGE,
  },
  {
    name: 'list_joined',
    description: 'List threads this node was invited into and does not own.',
    inputSchema: PAGE,
  },
  {
    name: 'describe_thread',
    description: 'Describe one thread this node can read, including members.',
    inputSchema: {
      type: 'object',
      required: ['owner', 'id'],
      properties: THREAD_REF,
    },
  },
  {
    name: 'list_messages',
    description: 'List messages in a thread. attention=thread reads the full list this node may see.',
    inputSchema: {
      type: 'object',
      required: ['owner', 'id'],
      properties: {
        ...THREAD_REF,
        attention: { type: 'string', enum: ['inbox', 'thread'], description: 'Default inbox (obligations); thread lists stored entries.' },
        page_size: { type: 'integer', minimum: 1, maximum: 100 },
        cursor: { type: 'string' },
      },
    },
  },
  {
    name: 'list_neighbors',
    description: 'List this node\'s trust-table neighbors (key-exchange rows).',
    inputSchema: { type: 'object', properties: {} },
  },
  {
    name: 'list_resources',
    description: 'List resources this node owns.',
    inputSchema: PAGE,
  },
  {
    name: 'describe_resource',
    description: 'Describe one resource by owner and id.',
    inputSchema: {
      type: 'object',
      required: ['owner', 'id'],
      properties: {
        owner: { type: 'string', description: 'Resource owner node id.' },
        id: { type: 'string', description: 'Resource id.' },
      },
    },
  },
  {
    name: 'node_status',
    description: 'This node\'s identity, endpoint, inbox size and neighbor count.',
    inputSchema: { type: 'object', properties: {} },
  },
] as const;

export const RESOURCES = [
  { uri: 'onlyne://status', name: 'status', mimeType: 'application/json', description: 'Node identity and queue counts.' },
  { uri: 'onlyne://inbox', name: 'inbox', mimeType: 'application/json', description: 'Local inbox snapshot.' },
  { uri: 'onlyne://threads', name: 'threads', mimeType: 'application/json', description: 'Threads this node owns.' },
  { uri: 'onlyne://neighbors', name: 'neighbors', mimeType: 'application/json', description: 'Trust-table neighbors.' },
] as const;

const str = (value: unknown): string | undefined =>
  typeof value === 'string' && value.length > 0 ? value : undefined;

const pageOptions = (args: Record<string, unknown>): { page_size?: number; cursor?: string } => ({
  ...(typeof args.page_size === 'number' ? { page_size: args.page_size } : {}),
  ...(typeof args.cursor === 'string' && args.cursor.length > 0 ? { cursor: args.cursor } : {}),
});

function result(data: unknown, isError = false): CallToolResult {
  return {
    content: [{ type: 'text', text: JSON.stringify(data, null, 2) }],
    ...(isError ? { isError: true } : {}),
  };
}

function failure(error: unknown): CallToolResult {
  if (error instanceof OnlyneError) {
    return result({ error: error.error }, true);
  }
  return result({ error: { code: 'unavailable', message: error instanceof Error ? error.message : String(error) } }, true);
}

async function statusOf(node: OnlyneNode): Promise<Json> {
  const inbox = await node.messages.inbox(node.auth);
  const threads = await node.threads.list(node.auth);
  const neighbors = await node.trust.list(node.auth);
  return {
    node_id: node.auth.actor,
    inbox: inbox.items.length,
    threads: threads.items.length,
    neighbors: neighbors.length,
    inbox_more: inbox.next_cursor !== null,
    threads_more: threads.next_cursor !== null,
  };
}

async function dispatch(node: OnlyneNode, name: string, args: Record<string, unknown>): Promise<CallToolResult> {
  const auth = node.auth;
  switch (name) {
    case 'send_message': {
      const to = str(args.to);
      const msg = str(args.msg);
      if (to === undefined || msg === undefined) {
        throw new OnlyneError('invalid_request', 'send_message needs to and msg.');
      }
      const threadOwner = str(args.thread_owner);
      const threadId = str(args.thread_id);
      const thread: ThreadRef | undefined = threadOwner !== undefined && threadId !== undefined
        ? { owner: threadOwner, id: threadId } : undefined;
      const replySender = str(args.in_reply_to_sender);
      const replyId = str(args.in_reply_to_id);
      const inReplyTo: MessageRef | undefined = replySender !== undefined && replyId !== undefined
        ? { sender: replySender, id: replyId } : undefined;
      const response = args.response === 'async' ? 'async' : 'none';
      const execution = args.execution === 'new' ? 'new' : 'existing';
      const plan: DeliveryPlan = { response, execution };
      const message: Message = {
        id: crypto.randomUUID(),
        sender: node.auth.actor,
        delivery_kind: str(args.delivery_kind) ?? 'message',
        msg,
        ...(thread === undefined && inReplyTo === undefined ? {} : {
          meta: {
            ...(thread === undefined ? {} : { thread }),
            ...(inReplyTo === undefined ? {} : { in_reply_to: inReplyTo, relation: 'reply' as const }),
          },
        }),
      };
      const outcome = await node.client.send(to, message, plan);
      return result({
        admission: outcome.admission,
        ...(outcome.receipt === undefined ? {} : { receipt: outcome.receipt }),
        ...(outcome.error === undefined ? {} : { error: outcome.error }),
        ...(outcome.resolved === undefined ? {} : { resolved: outcome.resolved }),
      });
    }
    case 'list_inbox':
      return result(await node.messages.inbox(auth, pageOptions(args)));
    case 'read_message': {
      const sender = str(args.sender);
      const id = str(args.id);
      if (sender === undefined || id === undefined) {
        throw new OnlyneError('invalid_request', 'read_message needs sender and id.');
      }
      return result(await node.messages.getByRef(auth, { sender, id }));
    }
    case 'list_threads':
      return result(await node.threads.list(auth, pageOptions(args)));
    case 'list_joined':
      return result(await node.client.joinedThreads(pageOptions(args)));
    case 'describe_thread': {
      const owner = str(args.owner);
      const id = str(args.id);
      if (owner === undefined || id === undefined) {
        throw new OnlyneError('invalid_request', 'describe_thread needs owner and id.');
      }
      return result(await node.client.describeThread({ owner, id }));
    }
    case 'list_messages': {
      const owner = str(args.owner);
      const id = str(args.id);
      if (owner === undefined || id === undefined) {
        throw new OnlyneError('invalid_request', 'list_messages needs owner and id.');
      }
      const attention = args.attention === 'thread' ? 'thread' : 'inbox';
      return result(await node.messages.list(auth, { owner, id }, { ...pageOptions(args), attention }));
    }
    case 'list_neighbors':
      return result(await node.trust.list(auth));
    case 'list_resources':
      return result(await node.resources.list(auth, pageOptions(args)));
    case 'describe_resource': {
      const owner = str(args.owner);
      const id = str(args.id);
      if (owner === undefined || id === undefined) {
        throw new OnlyneError('invalid_request', 'describe_resource needs owner and id.');
      }
      return result(await node.resources.describe(auth, { owner, id }));
    }
    case 'node_status':
      return result(await statusOf(node));
    default:
      throw new OnlyneError('unsupported_delivery', t('error.unknownTool', { name }));
  }
}

async function readResource(node: OnlyneNode, uri: string): Promise<string> {
  const auth = node.auth;
  switch (uri) {
    case 'onlyne://status':
      return JSON.stringify(await statusOf(node), null, 2);
    case 'onlyne://inbox':
      return JSON.stringify(await node.messages.inbox(auth), null, 2);
    case 'onlyne://threads':
      return JSON.stringify(await node.threads.list(auth), null, 2);
    case 'onlyne://neighbors':
      return JSON.stringify(await node.trust.list(auth), null, 2);
    default:
      throw new OnlyneError('not_found', t('error.unknownResource', { uri }));
  }
}

/** Builds the MCP `Server` bound to one Onlyne node. Call `connect` with stdio or an in-memory transport. */
export function createOnlyneMcpServer(node: OnlyneNode): Server {
  const server = new Server(
    { name: 'onlyne', version: '3.1.0' },
    { capabilities: { tools: {}, resources: {} } },
  );
  server.setRequestHandler(ListToolsRequestSchema, async () => ({ tools: [...TOOLS] }));
  server.setRequestHandler(CallToolRequestSchema, async request => {
    const name = request.params.name;
    const args = (request.params.arguments ?? {}) as Record<string, unknown>;
    try {
      return await dispatch(node, name, args);
    } catch (error) {
      return failure(error);
    }
  });
  server.setRequestHandler(ListResourcesRequestSchema, async () => ({ resources: [...RESOURCES] }));
  server.setRequestHandler(ReadResourceRequestSchema, async request => {
    const uri = request.params.uri;
    try {
      const text = await readResource(node, uri);
      return { contents: [{ uri, mimeType: 'application/json', text }] };
    } catch (error) {
      const message = error instanceof OnlyneError ? error.error.message : String(error);
      throw new Error(message);
    }
  });
  return server;
}

/** Stdio transport for MCP clients. Logs must not go to stdout while this is connected. */
export async function connectStdio(server: Server): Promise<StdioServerTransport> {
  const transport = new StdioServerTransport();
  await server.connect(transport);
  return transport;
}
