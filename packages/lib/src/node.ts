import type { Auth, NodeOptions } from './types.js';
import { createContext } from './core.js';
import { ThreadService } from './threads.js';
import { MessageService } from './messages.js';
import { createResourceService } from './resources.js';
import type { ResourceService } from './resources.js';
import { DomainService } from './domains.js';
import { DiscoveryService } from './discovery.js';
import { OnlyneClient } from './client.js';
import { KeyService, TrustService } from './keys.js';
import { FetchTransport } from './transport.js';
import { createHttpHandler } from './http.js';

export class OnlyneNode {
  readonly threads: ThreadService;
  readonly messages: MessageService;
  readonly resources: ResourceService;
  readonly domains: DomainService;
  readonly discovery: DiscoveryService;
  readonly keys: KeyService;
  readonly trust: TrustService;
  readonly client: OnlyneClient;
  /** The transport this node signs with; a host that talks to peers itself must reuse it. */
  readonly transport: import('./types.js').Transport;
  readonly fetch: (request: Request) => Promise<Response>;
  readonly auth: Auth;

  constructor(options: NodeOptions) {
    const ctx = createContext(options);
    this.auth = { actor: options.node_id };
    this.threads = new ThreadService(ctx);
    this.messages = new MessageService(ctx);
    this.resources = createResourceService(ctx);
    this.keys = new KeyService(ctx);
    this.trust = new TrustService(ctx, this.keys);
    const transport = options.plugins.transport ?? new FetchTransport(
      options.plugins.resolve,
      fetch,
      options.plugins.keys === undefined ? undefined : { trust: this.trust },
    );
    this.domains = new DomainService(ctx, transport);
    this.discovery = new DiscoveryService(ctx);
    this.transport = transport;
    this.client = new OnlyneClient(ctx, transport, this.messages, this.threads, this.resources, this.domains);
    this.fetch = createHttpHandler({
      ctx, threads: this.threads, messages: this.messages, resources: this.resources, domains: this.domains, discovery: this.discovery,
      keys: this.keys, trust: this.trust,
    });
  }
}
