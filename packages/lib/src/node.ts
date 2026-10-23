import type { Auth, NodeOptions } from './types.js';
import { createContext } from './core.js';
import { ThreadService } from './threads.js';
import { MessageService } from './messages.js';
import { createResourceService } from './resources.js';
import type { ResourceService } from './resources.js';
import { OnlyneClient } from './client.js';
import { FetchTransport } from './transport.js';
import { createHttpHandler } from './http.js';

export class OnlyneNode {
  readonly threads: ThreadService;
  readonly messages: MessageService;
  readonly resources: ResourceService;
  readonly client: OnlyneClient;
  readonly fetch: (request: Request) => Promise<Response>;
  readonly auth: Auth;

  constructor(options: NodeOptions) {
    const ctx = createContext(options);
    this.auth = { actor: options.node_id };
    this.threads = new ThreadService(ctx);
    this.messages = new MessageService(ctx);
    this.resources = createResourceService(ctx);
    const transport = options.plugins.transport ?? new FetchTransport(options.plugins.resolve);
    this.client = new OnlyneClient(ctx, transport, this.messages, this.threads, this.resources);
    this.fetch = createHttpHandler({ ctx, threads: this.threads, messages: this.messages, resources: this.resources });
  }
}
