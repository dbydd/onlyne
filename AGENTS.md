# Onlyne v3 execution contract

Work on the `v3.0` branch. The workspace packages implement the v3 TypeScript library under the user's implementation instruction.

## Design constraints

- Use TypeScript libraries and packages. OMP is the first target runtime.
- Keep one agent-facing message entry: `msg({ delivery_kind, msg, meta })`. SystemOne interprets delivery intent.
- Keep the system decentralized. Each participant records its own handled exchanges. There is no central Onlyne server.
- Separate library semantics, protocol bindings, and runtime integration. A plugin API limitation does not redefine the library contract.
- Reuse A2A and other protocol abstractions. Include resources such as IM gateways and compute services, with explicit dynamic handover.
- Keep identity, permission checks, reply correlation, and handover commits deterministic.
- Keep filesystem access in host plugins. The library may use the network stack and must receive storage, configuration, credentials, and content access from its host.

## Current documents

The repository root is a Bun workspace. `packages/` holds publishable runtime code and nothing else; `packages/lib` is `@onlyne/lib`, whose `tests/verify.ts` exercises its public interfaces. `proof/` holds formal verification: `proof_rfc` models the draft specifications, `proof_lib` models the implementation. `docs/rfcs/` holds the draft v3 network service specifications. Run `bun run check` and `bun run test` from the repository root after integration.

The earlier consolidated discussion draft lives at `archive/design/v3-PROTOCOL-DRAFT.md`. The RFC set is the current design surface.

The archived v1/v2 code, designs, handbooks, issues, and release history live under `archive/`. Its execution contract applies within that directory. Use historical material as reference, not as v3 requirements.
