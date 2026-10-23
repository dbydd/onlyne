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

The model calls `node.client.msg({ delivery_kind, msg, meta })`. The `decide` plugin returns a typed send, read, thread, or resource action. An unavailable decision returns `decision_unavailable`. Hosts can also call the typed `threads`, `messages`, `resources`, and `client.send` interfaces directly.

## Plugin boundary

The library performs no filesystem operations. Persistence, configuration loading, credentials, attachment access, runtime integration, and platform operations belong to the host plugins. Message parts and resource data carry opaque JSON references. The library does not open the paths they contain.

| Plugin | Host responsibility |
|---|---|
| `storage` | Atomic, serializable transactions and the declared admission guarantee. A rejected transaction callback rolls back all writes. |
| `authenticate` | Verify the request credentials and return the actor plus any authorized forwarding identity. |
| `resolve` | Resolve a node identity to its service URL and request headers. |
| `decide` | Translate message intent into one executable action. |
| `runtime` | Check delivery support, inject into a host context, and optionally create a runtime todo. |
| `resources` | Validate resource parameters, execute platform operations, and query uncertain outcomes. |
| `mayAdmit`, `mayHandover`, `mention`, `directory` | Supply admission policy, handover policy, thread aliases, and the host directory. |
| `transport` | Optionally replace the built-in Fetch transport with another network binding. |

`MemoryStorage` is process-local and returns `binding-defined` receipts. A durable plugin returns `durable` after its transaction commit reliably stores the message, receipt, and obligations. The library does not upgrade a plugin's guarantee.

The host calls `node.client.maintain()` to retry grant notices, reconcile attention, and inject queued entries. It calls `node.client.recover()` to resume outbound requests and `node.resources.recover()` to settle resource recovery. The host chooses when these calls run and retains the storage plugin across node reconstruction. Runtime injection reports consumption before the library records an entry as injected.

Resource handover starts as `pending`. The named target accepts or rejects it through `decideHandover`. Commit checks the current revision, permissions, deadline, and delivery backlog. Platform SDKs, platform credentials, and artifact readers remain in resource plugins.
