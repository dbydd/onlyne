# Onlyne v3

Onlyne v3 is a decentralized TypeScript system for agent messaging and resource coordination. Agents express delivery intent through `msg({ delivery_kind, msg, meta })`. Each participant keeps its own records. There is no central Onlyne server, global ledger, or task projection.

The repository holds the draft protocol specifications, the runtime packages, and two formal-verification records.

## Packages

| Package | Contents |
|---|---|
| [`packages/lib`](packages/lib/README.md) | `@onlyne/lib`: threads, durable message admission, attention obligations, reply correlation, ownerless domains, resources with explicit handover, the HTTP binding, and the plugin interfaces |
| [`packages/node-host`](packages/node-host/README.md) | `@onlyne/node-host`: identity keys, DNS-SD discovery, the gateway trust terminal, and message catalogs |
| [`packages/im-core`](packages/im-core/README.md) | `@onlyne/im-core`: the IM connector contract and the driver bridge |
| [`packages/im-fake`](packages/im-fake/README.md) | `@onlyne/im-fake`: a scripted connector for tests |
| [`packages/im-lark`](packages/im-lark/README.md) | `@onlyne/im-lark`: the Lark (Feishu) connector |
| [`packages/im-gateway`](packages/im-gateway/README.md) | `@onlyne/im-gateway`: IM conversations hosted as resources, plus the trust terminal |
| [`packages/mcp-core`](packages/mcp-core/README.md) | `@onlyne/mcp-core`: the MCP session and resource driver |
| [`packages/mcp-gateway`](packages/mcp-gateway/README.md) | `@onlyne/mcp-gateway`: MCP servers hosted as resources |
| [`packages/omp-onlyne`](packages/omp-onlyne/README.md) | OMP integration: one `msg` tool, judge-selected delivery, and inbound injection |

`packages/` holds publishable runtime code only. Further packages sit beside `lib` as they land.

Every package ships TypeScript source for Bun, so `engines.bun` is the only runtime requirement.
Scoped packages publish with `access: public`.

## Publishing

The packages depend on each other, so publish in dependency order:

```sh
for pkg in lib node-host im-core im-fake im-lark im-gateway mcp-core mcp-gateway omp-onlyne; do
  npm publish "packages/$pkg"
done
```

`npm pack --dry-run packages/<name>` lists the files one package would ship. Each manifest
carries `files`, `repository.directory`, and a license, so the tarball holds `src`, the README,
and the LICENSE only.

## Formal verification

| Directory | Models |
|---|---|
| [`proof/proof_rfc`](proof/proof_rfc/SSOT.md) | The RFC-level formalization: admission, attention, and binding, with bounded state search over the draft rules |
| `proof/proof_lib` | The implementation-level formalization: the semantics the library actually runs |

`proof_rfc` checks the drafts. `proof_lib` checks the shipped implementation. Each record states its own bounds; a bounded search can refute a property inside its bounds and cannot prove one.

## RFC set

| RFC | Contents |
|---|---|
| [0000 Overview](docs/rfcs/0000-overview.md) | Goals, object ownership, invariants |
| [0001 Core](docs/rfcs/0001-core.md) | Identity, references, HTTP, errors, retries |
| [0002 Threads](docs/rfcs/0002-threads.md) | Thread lifecycle, membership, access |
| [0003 Messages](docs/rfcs/0003-messages.md) | Send, durable admission, receipts, replies |
| [0004 Views](docs/rfcs/0004-events-and-views.md) | Message list, indexing, event resumption |
| [0005 Delivery](docs/rfcs/0005-delivery-and-runtime.md) | SystemOne, library waiting, runtime todo |
| [0006 Resources](docs/rfcs/0006-resources-and-handover.md) | Resources, bindings, dynamic handover |
| [0007 Bindings](docs/rfcs/0007-bindings.md) | A2A, MCP, HTTP, OMP mappings |
| [0008 OMP surface](docs/rfcs/0008-omp-surface.md) | Agent tools, message views, injection, runtime todo |
| [0009 A2A reference](docs/references/a2a.md) | Upstream protocol, network operations, mapping boundaries |
| [0010 ANP reference](docs/references/anp.md) | Upstream identity, discovery, network protocols |
| [0011 Discovery](docs/rfcs/0011-discovery-and-domains.md) | Node discovery, ownerless domains, service description |
| [0012 IM resource node](docs/rfcs/0012-im-resource-node.md) | IM conversations: events, bindings, handover, recovery |
| [0013 Compute job node](docs/rfcs/0013-compute-job-resource.md) | Compute jobs: split authorizations, artifacts, recovery |
| [0014 Data source node](docs/rfcs/0014-data-source-resource.md) | Data sources: scoped authorization, subscriptions, recovery |
| [0015 MCP resource node](docs/rfcs/0015-mcp-resource-node.md) | MCP servers as stateless resource nodes |

- [Archived v1/v2 implementation and documentation](archive/README.md)
- [中文](README.zh-CN.md)

The RFCs remain drafts. The library implements the shared messaging, HTTP, runtime, and resource contracts. Platform adapters and OMP, A2A, ANP, and MCP integrations require host plugins.
