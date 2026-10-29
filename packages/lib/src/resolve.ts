import { OnlyneError } from './core.ts';
import type { Failure, NodeId, Target, TrustEntry } from './types.ts';

/**
 * The one place an address is composed for the wire (RFC 0011 §4, RFC 0018 §5.1).
 * The host resolver — usually the discovery catalog — answers first. When it reports
 * `not_found` (never observed) or `unavailable` (recorded but no address answers), a
 * trusted row's `base_url` is the authoritative fallback: probing it succeeds writes the
 * address back into the discovery catalog, so a peer that moved stays reachable and the
 * new address becomes visible to the model. A trusted row whose address also fails is
 * `unavailable` — never degraded to `not_found`, which names an unknown identity.
 */
export function composeResolver(deps: {
  resolve: (node: NodeId) => Promise<Target>;
  trust?: { entry(node: NodeId): Promise<TrustEntry | undefined> };
  observe?: (base_url: string, source: string, expectedNode?: NodeId) => Promise<unknown>;
}): (node: NodeId) => Promise<Target> {
  return async node => {
    try {
      return await deps.resolve(node);
    } catch (error) {
      const failure: Failure | undefined = error instanceof OnlyneError
        ? error.error
        : (error as { error?: Failure } | null)?.error;
      if (failure?.code !== 'not_found' && failure?.code !== 'unavailable') throw error;
      const entry = await deps.trust?.entry(node);
      if (entry === undefined || entry.state !== 'trusted' || entry.base_url.length === 0) throw error;
      if (deps.observe !== undefined) {
        try {
          await deps.observe(entry.base_url, 'trust-row', node);
        } catch {
          throw new OnlyneError('unavailable',
            `${node} holds a trusted row whose address is ${entry.base_url}, but no recorded address answered and that address did not verify either.`);
        }
      }
      return { node_id: node, base_url: entry.base_url };
    }
  };
}
