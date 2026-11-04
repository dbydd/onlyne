import { fail } from '../core.ts';
import { validateDomainId } from '../domains.ts';
import type { DomainMutation } from '../types.ts';
import { expect, json, mutation, optionalString, pageOptions, respond, shape, type RouteRequest } from './shared.ts';

/** Domain membership: the node's own declaration, its peers, and the candidate exchange. */
export async function domainRoutes({ request, path, params, ctx, domains, auth, rawBody, limit }: RouteRequest): Promise<Response | undefined> {
  if (path.length === 1 && path[0] === 'domains') {
    if (request.method === 'GET') return respond(await domains.list(auth, pageOptions(params)));
    if (request.method === 'POST') {
      const raw = shape(await json(request, limit, rawBody), 'body', ['idempotency_key', 'retry_until']);
      // Membership belongs to the node that holds it; a peer's credential never mutates it.
      ctx.owner(auth.actor);
      return respond(await domains.create(auth, mutation(request, raw)), 201);
    }
    fail('not_found', 'Unknown route.');
  }

  if (path.length === 2 && path[0] === 'domains') {
    expect(request, 'GET');
    return respond(await domains.describe(auth, validateDomainId(path[1])));
  }

  if (path.length === 3 && path[0] === 'domains') {
    const id = validateDomainId(path[1]);
    if (path[2] === 'members') {
      expect(request, 'GET');
      return respond(await domains.discover(auth, id, request.signal));
    }
    if (path[2] === 'join' || path[2] === 'leave') {
      expect(request, 'POST');
      const raw = shape(await json(request, limit, rawBody), 'body', ['idempotency_key', 'retry_until', 'expected_revision']);
      const expected = optionalString(raw.expected_revision, 'expected_revision');
      ctx.owner(auth.actor);
      const change: DomainMutation = {
        ...mutation(request, raw),
        ...(expected === undefined ? {} : { expected_revision: expected }),
      };
      return respond(await (path[2] === 'join' ? domains.join(auth, id, change) : domains.leave(auth, id, change)));
    }
    fail('not_found', 'Unknown route.');
  }

  return undefined;
}
