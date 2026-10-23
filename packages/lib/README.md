# @onlyne/lib

The Onlyne v3 library: threads, durable message admission, attention obligations, reply correlation, resources with explicit handover, and a Fetch-compatible HTTP binding.

The protocol is specified by the draft RFCs in [`docs/rfcs`](../../docs/rfcs). The RFC-level formalization lives in [`proof/proof_rfc`](../../proof/proof_rfc); the implementation-level one in `proof/proof_lib`.

## Run

The package exports TypeScript source for a TypeScript-capable host such as Bun.

```sh
bun install
bun run check
bun run test
```

`bun run test` runs the public API verification script, including its loopback HTTP nodes. From the repository root, `bun run check` and `bun run test` delegate to this package.

## Library

The host supplies the node identity, service URL, and plugins. The library exposes a Fetch handler. The host owns the listener and its shutdown.

```ts
import { OnlyneNode, type NodeOptions } from '@onlyne/lib';

export function serveOnlyne(options: NodeOptions, port: number) {
  const node = new OnlyneNode(options);
  const server = Bun.serve({
    hostname: '127.0.0.1',
    port,
    fetch: node.fetch,
  });
  return { node, server };
}
```

`options.base_url` includes `/onlyne/v3`. `FetchTransport` permits HTTP on loopback and requires HTTPS for remote hosts. The resolver supplies the remote identity, URL, and authentication headers. The authentication plugin maps each inbound request to its verified actor.

The model calls `node.client.msg({ delivery_kind, msg, meta })`. The `decide` plugin returns a typed send, read, thread, resource, or domain action. An unavailable decision returns `decision_unavailable`. Hosts can also call the typed `threads`, `messages`, `resources`, `domains`, and `client.send` interfaces directly.

A plan whose `response` is `await` keeps `client.send` on the reply. The outcome reports `wait` as `answered`, `timed_out`, or `cancelled`, and an answered one carries `reply` with `reply_body`. The awaited tool result and the automatic attention fragment share one consumption record per `(context, ThreadRef, seq)`. A body already confirmed in that context comes back as `reply_body: 'withheld'` with the arrival facts and the original MessageRef only; a body this result rendered closes the record, so the fragment does not present it again. `runtime.confirmToolResult` reports the tool result the host observed in that context. A record no path confirmed stays open and is replayed.

## Domains

A domain is a named group of nodes. It has no owner, no central member table, and no implied thread or resource permission. A node belongs to one domain at a time: joining another requires leaving the current one. Threads may span domains; resource reach still goes through the resource owner or an existing ingress. Each node records only its own membership:

```ts
const created = await node.domains.create(node.auth, mutation);
await node.domains.join(node.auth, "urn:onlyne:domain:research", mutation);
const members = await node.domains.discover(node.auth, "urn:onlyne:domain:research");
```

`create`, `join`, and `leave` change this node's record and require its own actor. `create` or `join` for a different domain while an active membership exists fails with `conflict`. `describe` returns the local record, including a `left` tombstone. `list` returns at most one active membership. `discover` asks the host's `domainCandidates(domain_id)` for candidate node identities, queries each candidate's `/domains/{domain_id}`, and returns only the nodes that currently declare themselves active. Failed lookups land in `unavailable`; no cache or peer list is treated as the member truth.

Membership survives a node rebuild over the same storage. A repeated `join` with the current revision is idempotent. A stale `expected_revision` is refused. Retrying an earlier mutation key returns the original result without reviving a later `leave`. The HTTP routes are `POST /domains`, `GET /domains`, `GET /domains/{domain_id}`, `POST /domains/{domain_id}/join`, `POST /domains/{domain_id}/leave`, and `GET /domains/{domain_id}/members`. An authenticated service description carries the single active id in `domain`; an unauthenticated one carries neither `domain` nor `domains`.

## Resources

An operation result is `committed`, `pending`, `aborted`, or `unknown`. `aborted` means the endpoint refused or locally stopped the call and confirmed that no external effect happened; a failure whose side effect cannot be ruled out is `unknown`, keeps the original error in `details`, and stays owed to `node.resources.recover()`, which asks the driver for the outcome by `operation_id`. A caller that stops waiting gets `unknown` while the external call keeps running, and a later driver outcome settles that record. A retried operation is asked for its outcome, never executed a second time.

An event carries its acknowledgement and its retention deadline. The `expired` terminal state and a recipient's `ack` each remove the same delivery obligation from the pending backlog, idempotently. Expiry ends the local delivery responsibility; the content still follows the content retention policy. An `ack` arriving after expiry keeps its receipt evidence without reviving the obligation, removing it a second time, or restoring the local delivery responsibility. That backlog is the condition handover checks.

Resource handover starts as `pending`. The named target accepts or rejects it through `decideHandover`. Commit checks the current revision, permissions, `decision_deadline`, and an empty delivery backlog. A prepare that expires before commit is `aborted` and cannot be completed from the acceptance already on record. Platform SDKs, platform credentials, and artifact readers remain in resource plugins.

## Plugin boundary

The library performs no filesystem operations. Persistence, configuration loading, credentials, attachment access, runtime integration, and platform operations belong to the host plugins. Message parts and resource data carry opaque JSON references. The library does not open the paths they contain.

| Plugin | Host responsibility |
|---|---|
| `storage` | Atomic, serializable transactions and the declared admission guarantee. A rejected transaction callback rolls back all writes. |
| `authenticate` | Verify the request credentials and return the actor plus any authorized forwarding identity. |
| `resolve` | Resolve a node identity to its service URL and request headers. |
| `decide` | Translate message intent into one executable action. |
| `runtime` | Check delivery support, inject into a host context, optionally create a runtime todo, and report an `await` tool result the host observed. |
| `resources` | Validate resource parameters, execute platform operations, and query uncertain outcomes. |
| `mayAdmit`, `mayHandover`, `mention`, `directory` | Supply admission policy, handover policy, thread aliases, and the host directory. |
| `domainCandidates` | List the node identities that may declare membership in a domain. Without it, discovery is refused rather than guessed. |
| `transport` | Optionally replace the built-in Fetch transport with another network binding. |

`MemoryStorage` is process-local and returns `binding-defined` receipts. A durable plugin returns `durable` after its transaction commit reliably stores the message, receipt, and obligations. The library does not upgrade a plugin's guarantee.

The host calls `node.client.maintain()` to retry grant notices, reconcile attention, and inject queued entries. It calls `node.client.recover()` to resume outbound requests and `node.resources.recover()` to settle resource recovery. The host chooses when these calls run and retains the storage plugin across node reconstruction. Runtime injection reports consumption before the library records an entry as injected.

The library stores no task state. Turning a message into a runtime todo is the host's: the todo snapshot and the message-reference association go into one recoverable commit, the host projection derives from that commit, and recovery replays it before creating another entry. `runtime.todo` is called once consumption is recorded; todo content and reminders stay with the host.
