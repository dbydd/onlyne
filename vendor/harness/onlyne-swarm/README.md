# onlyne-swarm

`onlyne-swarm` is a Rust scheduler for reactive multi-agent workflows. It maps each workspace to an agent, each task to a Pi session, and each `back_edges` entry to a loopback delivery edge. The graph can contain cycles. Users control long-running cycles with the CLI or TUI.

The scheduler uses the local [Onlyne](https://github.com/dbydd/onlyne) daemon as its workspace message layer. It creates one workspace-local daemon for every generated agent workspace and starts one Orca terminal with `pi` for each dispatched task.

## Runtime requirements

- macOS or Linux with Unix domain socket support
- Rust 1.85 or newer for source builds
- `onlyne` 0.4.x available as `onlyne` in `PATH`, or `ONLYNE_BIN` pointing to a built binary
- Orca 1.4 or newer with the `orca` CLI available in `PATH`
- Pi available as `pi` in `PATH`
- `pi-onlyne` installed in Pi and configured with `[swarm] enabled = true` in generated agent workspaces
- A configured Pi model and provider for real sessions

The scheduler itself has no model provider configuration. The `model` fields in schedule templates remain available as graph metadata for the session launcher and operator tooling.

macOS keeps a short path limit for Unix socket names. Use a short swarm root such as `/tmp/swarm` or `examples/swarm` when generated workspace paths approach the limit.

## Install

Install the published binary:

```bash
cargo install onlyne-swarm
```

Build from this repository:

```bash
cargo build --release
./target/release/onlyne-swarm --help
```

The package includes the Rust source, protocol documents, TUI document, and the `e2e/` stub runner.

## Examples

- [`examples/marquee`](examples/marquee/README.md): five-node relay ring (a→b→c→d→e→a) exercising reclaim, self-excitation, and unbounded whole-task replay.

## Create a swarm tree

Run commands from the swarm root. The current directory becomes the scheduler root and stays local to that scheduler instance.

The fastest path is the initializer (existing files are never overwritten):

```bash
onlyne-swarm init
onlyne-swarm export-skill
```

`init` creates a loopback-only `.onlyne/config.toml` with `[swarm]` on when
missing. When the directory is already an Onlyne workspace, `init` keeps
adapters and secrets and only flips `[swarm] enabled = true`. It also writes
a full `.agents/.schedule/planner/template.workspace.jsonc` starter, a root
`.onlyne/swarm.workspace.jsonc`, and `.onlyne/.env`.

Templates accept `name`, `role`, `model`, `back_edges`, plus a `$schema`
editor hint pointing at `template.workspace.schema.json`:

```text
swarm-root/
  .agents/.schedule/
    planner/template.workspace.jsonc
    reviewer/template.workspace.jsonc
```

Example template:

```jsonc
{
  "$schema": "https://raw.githubusercontent.com/dbydd/onlyne-swarm/main/template.workspace.schema.json",
  "name": "reviewer",
  "role": "Review the incoming task and return a concise result.",
  "model": {
    "provider": "openai-codex",
    "model": "gpt-5",
    "effort": "medium"
  },
  "back_edges": ["../planner"]
}
```

Relative `back_edges` resolve from the template directory. Missing targets fail synchronization. Cyclic edges remain valid.

Generate workspaces explicitly:

```bash
onlyne-swarm workspace create
# Reconcile descriptions after edits
onlyne-swarm workspace sync
```

The generated tree contains `.ws/`, local `.onlyne/` state, and `onlyne_in/` symlinks. Runtime state stays under the selected swarm root.

## Start and submit

Start the scheduler in the swarm root:

```bash
ONLYNE_BIN=/path/to/onlyne onlyne-swarm run
```

`run` synchronizes the tree, starts missing workspace daemons, subscribes to their priority event streams, and listens on `.onlyne/run/swarm.sock`.

Submit a Markdown payload from another shell in the same root:

```bash
cat > payload.md <<'EOF'
Inspect the current build and return a short report.
EOF
onlyne-swarm submit --to reviewer --payload payload.md
```

Useful operator commands:

```bash
onlyne-swarm status
onlyne-swarm list
onlyne-swarm list --state running
onlyne-swarm cancel <task-id> --reason "manual stop"
onlyne-swarm shell-completions zsh
onlyne-swarm shell-completions fish
```

`task_id` identifies the full task family. `cancel` terminates the selected family and its live terminals. Cyclic workflows have no automatic stop condition.

## Pi and pi-onlyne setup

Each generated agent workspace uses a local `.onlyne/config.toml` with loopback IO and `[swarm] enabled = true`. Install `pi-onlyne` through Pi's package manager:

```bash
pi install npm:pi-onlyne
```

For a generated workspace that should start watching when Pi opens, create `.pi/onlyne.json`:

```json
{
  "watch": { "autoStart": true },
  "outbound": {
    "defaultReplyMode": "explicit-only",
    "retry": { "attempts": 4, "concurrency": 8 }
  }
}
```

The scheduler starts Pi with normal extension discovery. The session sends `swarm_ready`, receives the persisted task through the follow-up queue, and finishes through `onlyne_swarm_reply` or `onlyne_mark_no_reply`.

## TUI

Open the monitor after `run` is active:

```bash
onlyne-swarm tui
```

The TUI shows workspace paths, back edges, task state, attempts, terminal handles, daemon health, pending callbacks, and dead letters. Use the documented key bindings in [TUI.md](TUI.md).

## Shell completions

```bash
onlyne-swarm shell-completions zsh
onlyne-swarm shell-completions fish
```

## Configuration and protocol

- [SPEC.md](SPEC.md) — frozen product and lifecycle semantics
- [ARCH.md](ARCH.md) — scheduler architecture
- [PROTOCOL.md](PROTOCOL.md) — `---swarm` message header
- [TEMPLATE.md](TEMPLATE.md) — template merge and edge resolution
- [template.workspace.schema.json](template.workspace.schema.json) — editor schema for full template keys
- [IPC.md](IPC.md) — `swarm.sock` operations
- [TUI.md](TUI.md) — monitor layout and controls
- [TEST.md](TEST.md) — verification scenarios

The scheduler persists task payloads and state in `.onlyne/swarm.db`. Message history remains in each workspace daemon's `.onlyne/state.db`.

## Testing

Run Rust unit tests:

```bash
cargo test
```

Run all protocol and lifecycle stubs:

```bash
E2E_ROOT=/tmp/swarm-e2e-check ./e2e/run_e2e.sh all
```

The stub suite covers a single chain, sequential fan-out, concurrent fan-out, a self-exciting cycle, and cancellation. Real Orca and Pi sessions require the runtime prerequisites above and a configured model provider.

## Migration from 0.1.x

0.2.0 breaks the wire protocol (`transfer_send_to` replaces `reply_to`),
the database schema (no `pending_replies`, no dead-letter table), and the
pi-onlyne tool surface. Old instance directories are not migrated: delete
`.ws/` and `.onlyne/swarm.db`, then run `workspace create` again.
Upgrade pi-onlyne to 0.7.0 in lockstep.

## Compatibility

`pi-onlyne >= 0.8.1` requires `onlyne-swarm >= 0.5.0`. The scheduler's
second task delivery writes `delivery: scheduler`; pi-onlyne uses this
structured header field as its claim gate. Raw relay and out wires omit it,
so arbitrary role prose stays independent from transport authentication.

## Reclaim Protocol

Normal terminal recovery uses **control down, ack up, session self-exit**:

1. Scheduler sends a header-only loopback control wire:

   ```text
   ---swarm-ctl
   op: recycle
   task_id: <task-id>
   reason: <done|failed|cancel|operator>
   ---
   ```

2. pi-onlyne intercepts this wire before the model sees it, sends
   `swarm_recycled {task_id, terminal_handle, reason}` to its workspace daemon,
   stops watching, clears the slot, and exits its own process.
3. Scheduler observes/polls the ack for up to five seconds, then closes the
   Orca tab regardless. Missing ack logs `recycle_no_ack`.

`swarm_quit` also sends the ack (`quit:<reason>`) and self-exits, so an
explicit quit immediately reaches `failed` in the ledger and never pins a
Running task. `cancel --force` skips the ack window and immediately closes
that task's Orca tab; it is the operator escape hatch.

## Release checks

Run these checks before publishing a version:

```bash
cargo fmt --check
cargo test
cargo publish --dry-run
```

The crate is published independently from the Onlyne root repository.

## Scope

`onlyne-swarm` owns local graph scheduling, task persistence, event routing, terminal lifecycle, and monitoring. Onlyne owns channel transport and local history. Pi and `pi-onlyne` own agent execution and model interaction.

## License

MIT
