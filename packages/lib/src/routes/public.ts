import { fail } from '../core.ts';
import type { Json, TrustStatement } from '../types.ts';
import { DEFAULT_BYTES, expect, json, pageOptions, respond, type PublicRouteRequest } from './shared.ts';

/**
 * The public description: identity, address, current region declaration and configured
 * authentication schemes. It carries no credential subject, message or resource.
 */
async function description({ ctx, domains, discovery }: PublicRouteRequest): Promise<Json> {
  const active = await domains.declared({ actor: ctx.node });
  return {
    protocol: 'onlyne/3',
    node_id: ctx.node,
    instance_id: ctx.instanceId,
    base_url: ctx.baseUrl,
    features: [
      'threads', 'messages', 'events', 'resources', 'domains',
      ...(discovery === undefined ? [] : ['discovery']),
      ...(ctx.options.plugins.executionContexts === undefined ? [] : ['contexts']),
    ],
    // The binding names only schemes its host configured; a credential's subject is not a scheme.
    authentication: ctx.options.authentication ?? [],
    domain: active?.domain_id ?? null,
    membership: active === null
      ? null
      : { domain_id: active.domain_id, node_id: active.node_id, state: active.state, revision: active.revision },
    limits: { request_bytes: ctx.options.request_bytes ?? DEFAULT_BYTES },
    idempotency_max_seconds: Math.floor((ctx.options.retry_max_ms ?? 86_400_000) / 1000),
  };
}

/** Routes that answer before any credential is checked: identity, address and the exchange transcript. */
export async function publicRoutes(deps: PublicRouteRequest): Promise<Response | undefined> {
  const { request, path, params, ctx, domains, discovery, keys, trust } = deps;

  if (path.length === 1 && path[0] === 'description') {
    expect(request, 'GET');
    return respond(await description(deps));
  }

  if (path.length === 1 && path[0] === 'discovery') {
    expect(request, 'GET');
    if (discovery === undefined) fail('not_found', 'Unknown route.');
    return respond(await discovery.list(pageOptions(params)));
  }

  // RFC 0017: the identity lookup a first exchange reads. Public by design — a public key
  // is not a secret, and the fingerprint is what the two users compare out of band.
  if (path.length === 1 && path[0] === 'keys') {
    expect(request, 'GET');
    const identity = keys === undefined ? undefined : await keys.describe();
    if (identity === undefined) fail('not_found', 'This node has no identity key.');
    return respond(identity);
  }

  // The exchange transcript: each side signs (sender, recipient, key, base_url, approval,
  // timestamp, nonce), so a captured statement cannot be replayed or retargeted.
  if (path.length === 1 && path[0] === 'introductions') {
    expect(request, 'POST');
    if (trust === undefined) fail('not_found', 'Unknown route.');
    const limit = ctx.options.request_bytes ?? DEFAULT_BYTES;
    return respond(await trust.introduce(await json(request, limit) as TrustStatement), 201);
  }

  return undefined;
}
