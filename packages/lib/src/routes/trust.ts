import { fail } from '../core.ts';
import { expect, json, respond, shape, string, type RouteRequest } from './shared.ts';

/** The execution-context directory and the RFC 0017 trust transcript, both owner-scoped. */
export async function trustRoutes(deps: RouteRequest): Promise<Response | undefined> {
  const { request, path, ctx, trust, auth, rawBody, limit } = deps;

  if (path.length === 1 && path[0] === 'contexts') {
    expect(request, 'GET');
    const contexts = ctx.options.plugins.executionContexts;
    if (contexts === undefined) fail('unsupported_delivery', 'The host provides no execution context directory.');
    return respond(await contexts());
  }

  if (path.length >= 1 && path[0] === 'trust' && trust !== undefined) {
    if (path.length === 1) {
      expect(request, 'GET');
      return respond(await trust.list(auth));
    }
    if (path.length === 2 && path[1] === 'requests') {
      expect(request, 'POST');
      const raw = shape(await json(request, limit, rawBody), 'body', ['node_id', 'fingerprint']);
      return respond(await trust.request(auth, string(raw.node_id, 'node_id'), string(raw.fingerprint, 'fingerprint')), 201);
    }
    if (path.length === 2 && path[1] === 'rotate') {
      expect(request, 'POST');
      return respond(await trust.rotate(auth));
    }
    if (path.length === 3 && path[1] === 'revoke') {
      expect(request, 'POST');
      await trust.revoke(auth, path[2]!);
      return respond({ node_id: path[2]!, state: 'revoked' });
    }
    fail('not_found', 'Unknown route.');
  }

  return undefined;
}
