import { trustPhase, type NodeId, type OnlyneNode } from '@onlyne/lib';

/**
 * The rows `plugins.neighbors` serves (read scope `neighbors`, RFC 0004): every node
 * discovery has observed, annotated with its trust state, plus trusted rows discovery
 * never recorded. The same rows `onlyne://neighbors` renders on a node host.
 */
export async function neighborRows(node: OnlyneNode, owner: NodeId): Promise<unknown[]> {
  const records = await node.discovery.listRecords();
  const trust = new Map((await node.trust.list(node.auth)).map(entry => [entry.node_id, entry]));
  const at = Date.now();
  const byNode = new Map<string, Record<string, unknown>>();
  for (const record of records) {
    if (record.node_id === owner) continue;
    const entry = trust.get(record.node_id);
    const verified = record.verified_at !== undefined && Date.parse(record.expires_at) > at;
    const current = byNode.get(record.node_id) as { verified?: boolean } | undefined;
    if (current !== undefined && (current.verified || !verified)) continue;
    byNode.set(record.node_id, {
      node_id: record.node_id, base_url: record.base_url,
      ...(record.membership?.domain_id === undefined ? {} : { region: record.membership.domain_id }),
      ...(record.last_seen === undefined ? {} : { last_seen: record.last_seen }),
      verified: verified || entry?.state === 'trusted',
      phase: entry === undefined ? 'no_exchange' : trustPhase(entry),
      fingerprint: entry?.fingerprint ?? null,
    });
  }
  for (const entry of trust.values()) {
    if (entry.node_id === owner || byNode.has(entry.node_id)) continue;
    byNode.set(entry.node_id, {
      node_id: entry.node_id, base_url: entry.base_url,
      verified: entry.state === 'trusted',
      phase: trustPhase(entry),
      fingerprint: entry.fingerprint,
    });
  }
  return [...byNode.entries()].sort(([a], [b]) => a.localeCompare(b)).map(([, row]) => row);
}
