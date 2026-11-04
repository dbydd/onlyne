import { OnlyneError, fail } from './core.ts';
import type { Context } from './core.ts';
import type { Auth } from './types.ts';
import type { ThreadService } from './threads.ts';
import type { MessageService } from './messages.ts';
import type { ResourceService } from './resources.ts';
import type { DomainService } from './domains.ts';
import type { DiscoveryService } from './discovery.ts';
import type { KeyService, TrustService } from './keys.ts';
import { SIGNATURE_HEADER, bodyDigest } from './keys.ts';
import { DEFAULT_BYTES, respond, type PublicRouteRequest, type RouteRequest } from './routes/shared.ts';
import { publicRoutes } from './routes/public.ts';
import { trustRoutes } from './routes/trust.ts';
import { threadRoutes } from './routes/threads.ts';
import { grantRoutes } from './routes/grants.ts';
import { domainRoutes } from './routes/domains.ts';
import { resourceRoutes } from './routes/resources.ts';

const PREFIX = '/onlyne/v3';

export interface HttpServices {
  ctx: Context;
  threads: ThreadService;
  messages: MessageService;
  resources: ResourceService;
  domains: DomainService;
  discovery?: DiscoveryService;
  keys?: KeyService;
  trust?: TrustService;
}

function failure(error: unknown): Response {
  const onlyne = error instanceof OnlyneError ? error : new OnlyneError('internal', 'The node failed to complete the request.');
  return respond({ error: onlyne.error }, onlyne.status);
}

/** Maps `/onlyne/v3` HTTP requests onto the services. The caller owns the server lifecycle. */
export function createHttpHandler(services: HttpServices): (request: Request) => Promise<Response> {
  const { ctx, threads, messages, resources, domains, discovery, keys, trust } = services;
  const limit = ctx.options.request_bytes ?? DEFAULT_BYTES;

  /**
   * RFC 0017: an `Onlyne-Sig` header is verified against the peer's stored key before the
   * host plugin sees the request; a verified signature names the actor directly. The
   * signature covers the body, so it is checked against the raw bytes actually received.
   */
  async function authenticate(request: Request, rawBody: string): Promise<Auth> {
    // A keyed node never falls back to an unauthenticated actor: the signature is the credential.
    if (trust !== undefined && keys?.enabled === true) return trust.authenticate(request, bodyDigest(rawBody));
    const auth = await ctx.options.plugins.authenticate(request);
    if (auth === null || typeof auth.actor !== 'string' || auth.actor.length === 0) fail('unauthenticated', 'Authentication is required.');
    return auth;
  }

  /** RFC 0017: an authenticated answer to a signed request carries the responder's signature. */
  async function signed(request: Request, response: Response): Promise<Response> {
    if (trust === undefined || keys?.enabled !== true || request.headers.get(SIGNATURE_HEADER) === null) return response;
    await trust.signResponse(request, response, await response.clone().text());
    return response;
  }

  async function handle(request: Request): Promise<Response> {
    try {
      const url = new URL(request.url);
      if (!url.pathname.startsWith(`${PREFIX}/`)) fail('not_found', 'Unknown route.');
      const path = url.pathname.slice(PREFIX.length).split('/').filter(segment => segment.length > 0).map(segment => {
        try {
          return decodeURIComponent(segment);
        } catch {
          return fail('invalid_request', 'The request path is not valid percent-encoding.');
        }
      });
      const params = url.searchParams;

      const base: PublicRouteRequest = { request, path, params, ctx, threads, messages, resources, domains, discovery, keys, trust, limit };
      const open = await publicRoutes(base);
      if (open !== undefined) return open;

      // Authenticated routes read the raw body first: the signature covers exactly these bytes.
      const rawBody = await request.text();
      const auth = await authenticate(request, rawBody);
      const route: RouteRequest = { ...base, auth, rawBody };

      for (const module of [trustRoutes, threadRoutes, grantRoutes, domainRoutes, resourceRoutes]) {
        const response = await module(route);
        if (response !== undefined) return response;
      }

      fail('not_found', 'Unknown route.');
    } catch (error) {
      return failure(error);
    }
  }

  return async (request: Request): Promise<Response> => {
    const response = await handle(request);
    try { return await signed(request, response); }
    catch (error) { return failure(error); }
  };
}
