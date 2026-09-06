# Swarm live fixture

This directory is a short-path workspace root for a real Orca + pi + pi-onlyne session.

The scheduler reads `.agents/.schedule/a/template.workspace.jsonc` and generates `_onlyne_workspaces/a`.
Generated runtime state stays ignored.

Build the binaries from the repository root, then start the scheduler:

```bash
cargo build
cargo build --manifest-path harness/onlyne-swarm/Cargo.toml
cd examples/swarm
ONLYNE_BIN=../../target/debug/onlyne ../../harness/onlyne-swarm/target/debug/onlyne-swarm run
```

For an automatic pi watch, create `_onlyne_workspaces/a/.pi/onlyne.json` with `watch.autoStart=true`, then submit a payload:

```bash
mkdir -p _onlyne_workspaces/a/.pi
printf '{"watch":{"autoStart":true}}\n' > _onlyne_workspaces/a/.pi/onlyne.json
printf 'live payload marker\n' > payload.md
../../harness/onlyne-swarm/target/debug/onlyne-swarm submit --to a --payload payload.md
```

A real pi terminal reports `swarm_ready`, receives the persisted payload through its followUp queue, and completes with `onlyne_swarm_reply`.
