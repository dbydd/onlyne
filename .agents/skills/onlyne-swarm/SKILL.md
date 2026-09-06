---
name: onlyne-swarm
description: Use when the operator agent needs to create, configure, inspect, or drive a local onlyne-swarm multi-agent graph.
---

# onlyne-swarm supervisor skill

## Audience

This skill is for the operator agent that owns the swarm root. It creates
workspace descriptions, syncs the tree, starts the scheduler, submits work,
and watches progress. It is not for an agent session running inside one
generated workspace. A worker session only reads its own task from the
follow-up queue and finishes with the plugin reply tools.

## Mental model

- Swarm root = scheduler cwd. Commands run at the root; nested starts refuse.
- Description = `.agents/.schedule/<path>/template.workspace.jsonc`.
- Instance = `.ws/<path>`, generated one way from descriptions.
- Root = supervisor workspace (`.`). It submits, watches the ledger, cancels.
- Agent workspace = generated worker. The scheduler owns its daemon, terminal,
  Pi session, and out routing.
- Task = Pi session (`task_id == session_id`). One session carries one hop.
- Downstream work = new tasks spawned with `transfer_send_to` lineage.
  Sessions never wait; results travel through files and the ledger.
- Driving work inside a task belongs to the pi-onlyne plugin tools, not to
  shell commands from the operator.

## Prerequisites

- `onlyne` reachable (`onlyne` in `PATH`, or `ONLYNE_BIN` set).
- `onlyne-swarm` reachable in `PATH`.
- Orca running with the `orca` CLI reachable.
- `pi` reachable with `pi-onlyne` installed.
- A configured Pi model/provider for real sessions.
- Short root path on macOS (Unix socket names have a short limit).

## Initialize a root

```bash
onlyne-swarm init
```

Behavior:

- Missing `.onlyne/config.toml` is created loopback-only with `[swarm]` on.
- An existing Onlyne workspace keeps adapters and secrets; `init` only flips
  `[swarm] enabled = true`.
- Missing `.agents/.schedule/planner/template.workspace.jsonc`,
  `.onlyne/swarm.workspace.jsonc`, and `.onlyne/.env` are created as a full
  starter. Existing files are never overwritten.
- Every template key ships with its default; delete anything unchanged.

## Write templates

Full starter keys: `$schema`, `name`, `role`, `model`, `back_edges`.

```jsonc
{
  "$schema": "https://raw.githubusercontent.com/dbydd/onlyne-swarm/main/template.workspace.schema.json",
  "name": "planner",
  "role": "You are the planner. Return a concise result.",
  "model": { "provider": "", "model": "", "effort": "" },
  "back_edges": []
}
```

Rules:

- Directory nesting is the tree: `a/b/template.workspace.jsonc` is `a/b`.
- Keep names path-safe; `.` means the root supervisor.
- `back_edges` are tree paths relative to the declaring workspace
  (`../reviewer`, `sibling/worker`). Cyclic edges are allowed.
- Missing edge targets fail `sync`; fix the path, do not invent retries.
- Ancestor templates contribute scalar fields only; `back_edges` never inherit.
- Hand-tune one instance in
  `.ws/<path>/.onlyne/swarm.workspace.jsonc`; sync keeps it.
- Validate templates against the GitHub-hosted `template.workspace.schema.json`.

## Generate and inspect the tree

```bash
onlyne-swarm workspace create
onlyne-swarm workspace sync
onlyne-swarm list --what workspaces
onlyne-swarm status
```

`sync` never deletes instances or hand-written overlays. Removed descriptions
stay on disk as `orphan-instance` alerts. Missing daemon sockets show as
`dangling-link` alerts. Generated agents use loopback only; external channels
stay on the root unless the operator enables them there.

## Run and submit

```bash
ONLYNE_BIN=/path/to/onlyne onlyne-swarm run
```

Then from another shell in the same root:

```bash
cat > payload.md <<'EOF'
Inspect the current build and return a short report.
EOF
onlyne-swarm submit --to planner --payload payload.md
onlyne-swarm list
onlyne-swarm list --state running
```

`run` syncs once, starts missing daemons, subscribes to priority event
streams, and serves `.onlyne/run/swarm.sock`. Each dispatch opens one Orca
terminal with `pi`. The session sends `swarm_ready`, receives the persisted
payload through follow-up, and completes through the plugin reply tools.

## Monitor and cancel

```bash
onlyne-swarm status
onlyne-swarm list --state running
onlyne-swarm tui
onlyne-swarm cancel <task-id> --reason "manual stop"
```

`task_id` names the whole lineage family. Cyclic graphs run until the operator
cancels; there is no timeout, retry, or loop breaker. Cancelled tasks record
`swarm-cancelled` ledger rows; an exited session records `swarm-failed`.
Ledger tail is visible from `status` and the TUI.

## Refresh this skill

```bash
onlyne-swarm export-skill
```

The file lives at `.agents/skills/onlyne-swarm/SKILL.md` under the swarm
root. It is workspace-local; it never touches global skill directories.

## Common mistakes

- Starting the scheduler inside `.ws/...` instead of the root.
- Editing generated `.ws/` configs instead of the templates,
  then expecting `sync` to propagate the edit.
- Hand-writing a task header into loopback input instead of using `submit`.
- Treating `onlyne_in/<target>` as storage; it is a send-side symlink view.
- Opening the worker session manually and driving follow-ups by hand instead
  of letting the scheduler and plugin tools own delivery.
- Expecting the scheduler to retry failures; retry belongs to Pi-side plugins.
