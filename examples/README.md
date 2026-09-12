# Examples

The pre-v1 daemon is gone, and its example set went with it. The root `Cargo.toml` is a workspace
manifest, so `cargo run --example <name>` finds nothing to build here.

## End-to-end scripts

| script | covers |
| --- | --- |
| `crates/onlyne-testkit/e2e/local-task.sh` | one server, one role client, one fake agent, one task from send to acked |
| `crates/onlyne-testkit/e2e/two-cluster.sh` | parent server plus child server, with an aggregate role answering across the boundary |
| `crates/onlyne-testkit/e2e/gateway-mount.sh` | fake gateway registration and inbound routing to a role |
| `crates/onlyne-testkit/e2e/idempotency.sh` | duplicate and conflict answers for one repeated `op_id` |
| `crates/onlyne-testkit/e2e/legacy-layout.sh` | refusal of a pre-v1 workspace, exit code 2 |
| `crates/onlyne-testkit/e2e/running-lights.sh` | a six-role ring: a token handed on with `onlyne handoff` twelve times, and two TUI frames of it moving |

`crates/onlyne-testkit/e2e/lib.sh` holds the shared helpers. Callers set `SRC` and `tmp` first. Every
script runs with `ONLYNE_BACKEND=fake` and the `fake` gateway. No real platform credential enters
the run.

```bash
cargo build --workspace
cd crates/onlyne-testkit/e2e
bash local-task.sh
```

## Configuration example tree

`.onlyne.example/` holds three things: a comment-dense `spec.toml`, a `templates/dev/` tree for the
planner, builder, and reviewer roles, and a README that maps every example file to its real path.

## CLI vocabulary

`docs/v1-CONTRACT.md` lists the verbs for each binary: `onlyne-server run|init|reload|generate`,
`onlyne-client run|init`, `onlyne send|reply|complete|handoff|control`, and
`onlyne gateway run|list|status|auth`.
