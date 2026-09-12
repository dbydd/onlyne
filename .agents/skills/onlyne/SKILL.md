---
name: onlyne
description: Use when developing the Onlyne repository itself — touching crates, the wire protocol, ledger schema, CLI verbs, e2e proofs, or the verification gates.
---

# Onlyne Development

Guidance for agents changing this codebase. Runtime operation lives in
`skills/onlyne-supervisor/SKILL.md` and `skills/onlyne-role/SKILL.md`; this file covers
working on the repo.

## Product boundary (AGENTS.md §0)

Onlyne is transport: routing, ledger, queueing, ACL, session mechanics. Orchestration
belongs to supervisor sessions and the spec file. The core detects and records; the
operator decides retries, recycling, and timeouts. A feature that starts deciding policy
belongs in a supervisor session, the spec, or a plugin — pick one of those before adding
code here. Zero compatibility is a product rule: old configs, old databases, and old wire
versions fail at the door (`exit 2`, verbatim strings). A change that "also reads the old
shape" gets rejected.

## Crate map and dependency law

```text
onlyne-frame     4-byte BE length + JSON; zero business imports
onlyne-proto     types + validation + error codes; no tokio
onlyne-config    TOML spec/config parsing, env secrets
onlyne-layout    workspace/server root discovery, legacy refusal
onlyne-store     server ledger + client db; (generation,seq) monotonic gates
onlyne-session   pure lifecycle reducer + SessionBackend (orca|zellij|exec|fake)
onlyne-net       TLS 1.3 + pinning, ed25519 challenge, acl_allows, backoff
onlyne-adapter   the one adapter protocol SDK (agent side and gateway side)
onlyne-server/-client/-gateway   three bins; onlyne-cli the thin entry; onlyne-testkit fakes+e2e
plugins/onlyne-gateway-*         one platform per crate; depend on adapter+proto only
```

`onlyne-server` and `onlyne-client` never depend on each other. Platform SDKs
(`teloxide`, `openlark`, `wechat-ilink`, `resvg`) must stay out of both binaries. After any
dependency edit, prove it with
`cargo tree -p onlyne-client | grep -E 'teloxide|openlark|resvg'`. Gateways compile per
feature; `--no-default-features --features telegram` must build.

## Change procedures

**Wire or types** (`onlyne-proto`): edit the type, regenerate the schemas
(`cargo run -p onlyne-proto --bin gen-schema`), then update every affected fixture under
`crates/onlyne-proto/tests/wire_vectors/` (one JSON per reachable frame and error code).
Restate the contract in `crates/onlyne-adapter/PROTOCOL.md`. Error codes are a closed set
of fourteen. Add one and you owe a fixture, a PROTOCOL.md row, and the CLI table below.

**Lifecycle** (`onlyne-session/src/lifecycle.rs`): `apply()` and `is_legal()` are a
table-tested reducer — five state axes, 21 `LifecycleEvent` variants, versions
`(generation, seq)`. A new transition needs its table rows in the same commit. Reviewers
halt on weakened assertions during a migration.

**Ledger/schema** (`onlyne-store`): `schema_marker(name, version, protocol_version)` is the
gate. A field change bumps the marker and leaves the refuse-at-door string untouched.
`acl_allows` runs before the ledger write, so a denied send leaves zero rows and zero
sender-side intents. The built-in exemption covers only completions addressed to the
recorded task origin. Widening it needs a spec decision first.

**CLI verb** (`onlyne-cli`): args live in `verbs.rs`/`admin.rs`, out comes one JSON line.
Exit codes: `0` answer ok, `1` failed daemon answer or `wait-ready` bound, `2` validation,
`3` no socket, `4` generate refusal, `127` missing sibling. Socket resolution order stays
`--socket` → `--server-root` (admin) → `--workspace`/cwd walk (client). `--from` belongs to
the admin surface only; every message verb already prints JSON.

**Backend** (`onlyne-session/src/backend/`): capabilities `{spawn,attach,probe,close,
focus,rename}`. A missing capability degrades through faults, never panics. Discovery order
is `zellij → orca → fake` when `ONLYNE_BACKEND` is empty.

## Gates

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                 # per-crate -p reruns suffice for isolated edits
crates/onlyne-testkit/e2e/<case>.sh    # ONLYNE_BACKEND=fake, built target/debug, no real creds
```

The twelve scripts under `crates/onlyne-testkit/e2e/` each encode one verification case
from `docs/v1-PLAN.md` (ACL rejects, idempotency, reconnect requeue, gateway mount,
relocation, two-cluster federation, legacy refusal, frame bounds). A bug fix needs its
reproduction as an e2e or a table test: red before the fix, green after. The live ring demo
(`examples/supervisor/run.py`) needs Orca and real pi binaries. Treat it as manual smoke.

## Formal invariants

`proofs/` (GrugMatic, core Lean 4.33.1, zero dependencies) carries one combinator lemma per
design decision: D4 carrier bounds, D5 authority split, D6 content by reference, D11
idempotence, D12 delivery-creates-task, D13 file truth, D15 single-source prose, one-shot
sessions. When a change touches one of those invariants, read the lemma's docstring first.
If the change breaks the lemma, update `proofs/` in the same commit and keep
`cd proofs && lake build` green with zero `sorry`.
