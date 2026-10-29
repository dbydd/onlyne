import type { Auth, NodeOptions } from './types.ts';
import { createContext } from './core.ts';
import { ThreadService } from './threads.ts';
import { MessageService } from './messages.ts';
import { createResourceService } from './resources.ts';
import type { ResourceService } from './resources.ts';
import { DomainService } from './domains.ts';
import { DiscoveryService } from './discovery.ts';
import { OnlyneClient } from './client.ts';
import { KeyService, TrustService } from './keys.ts';
import { FetchTransport } from './transport.ts';
import { composeResolver } from './resolve.ts';
import { createHttpHandler } from './http.ts';
import { HistoryService } from './history.ts';

export class OnlyneNode {
  readonly threads: ThreadService;
  readonly messages: MessageService;
  readonly history: HistoryService;
  readonly resources: ResourceService;
  readonly domains: DomainService;
  readonly discovery: DiscoveryService;
  readonly keys: KeyService;
  readonly trust: TrustService;
  readonly client: OnlyneClient;
  /** The transport this node signs with; a host that talks to peers itself must reuse it. */
  readonly transport: import('./types.ts').Transport;
  readonly fetch: (request: Request) => Promise<Response>;
  readonly auth: Auth;

  constructor(options: NodeOptions) {
    const ctx = createContext(options);
    this.auth = { actor: options.node_id };
    this.threads = new ThreadService(ctx);
    this.messages = new MessageService(ctx);
    this.history = new HistoryService(ctx);
    this.keys = new KeyService(ctx);
    this.trust = new TrustService(ctx, this.keys);
    this.discovery = new DiscoveryService(ctx);
    const transport = options.plugins.transport ?? new FetchTransport(
      // The composed resolver is the one address authority (resolve.ts): discovery first,
      // the trust row as the fallback that writes a moved peer's address back.
      composeResolver({
        resolve: options.plugins.resolve,
        trust: this.trust,
        observe: (base_url, source, expectedNode) => this.discovery.observe(base_url, source, expectedNode),
      }),
      fetch,
      options.plugins.keys === undefined ? undefined : { trust: this.trust },
    );
    // A handover probe is a signed peer request, so the resource service shares the node's
    // transport instead of reaching the network on its own.
    this.resources = createResourceService(ctx, transport);
    this.domains = new DomainService(ctx);
    this.transport = transport;
    this.client = new OnlyneClient(ctx, transport, this.messages, this.threads, this.resources, this.domains, this.trust);
    this.trust.bindLoop(this.client);
    this.fetch = createHttpHandler({
      ctx, threads: this.threads, messages: this.messages, resources: this.resources, domains: this.domains, discovery: this.discovery,
      keys: this.keys, trust: this.trust,
    });
  }
}
