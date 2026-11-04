import { fail } from '../core.ts';
import { expect, grant, json, mutation, pageOptions, respond, shape, type RouteRequest } from './shared.ts';

/**
 * Grant notices in both directions: the owner's push and the recipient's accepted copy, the
 * owner's issued-membership listing a rebuilt participant repairs from, and the ack that
 * closes one notice.
 */
export async function grantRoutes({ request, path, params, threads, auth, rawBody, limit }: RouteRequest): Promise<Response | undefined> {
  if (path.length === 1 && path[0] === 'grants') {
    if (request.method === 'GET') return respond(await threads.grants(auth, pageOptions(params)));
    if (request.method === 'POST') {
      const raw = shape(await json(request, limit, rawBody), 'body', ['grant']);
      return respond(await threads.acceptGrant(auth, grant(raw.grant)), 201);
    }
    fail('not_found', 'Unknown route.');
  }

  // The memberships this node still holds for the caller. A participant whose own store
  // lost its grant rows asks here; an older owner answers not_found, which the caller
  // records as a watermark instead of asking again inside the window.
  if (path.length === 2 && path[0] === 'grants' && path[1] === 'issued') {
    expect(request, 'GET');
    return respond(await threads.issued(auth, pageOptions(params)));
  }

  if (path.length === 3 && path[0] === 'grants' && path[2] === 'ack') {
    expect(request, 'POST');
    const raw = shape(await json(request, limit, rawBody), 'body', ['idempotency_key', 'retry_until']);
    return respond(await threads.ackGrant(auth, path[1]!, mutation(request, raw)));
  }

  return undefined;
}
