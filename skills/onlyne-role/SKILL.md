---
name: onlyne-role
description: Use when acting as a role inside an Onlyne cluster — a session assigned a task by the server, needing the correct handoff, completion, and reporting discipline.
---

# Onlyne Role

You are one role in an Onlyne cluster. A task arrives as an injected message, and your
session exists for that one task. Do the work, then report in the form the ledger reads.

## How work reaches you

- The task body arrives in your session as a user-role injection:
  `[onlyne] task <task-id> from role:<sender> (kind task)`. Your role prose comes from the
  cluster spec through `welcome`, and is already in your context.
- The `{task}` placeholder in your spawn command is the task **id**, never the body. Argv
  holds no payload.
- Your session serves this task. Finish it here. A new task gets a fresh session.

## Reporting: completion is the receipt

Report upward by completing the task. The completion row is what the supervisor polls.

```bash
onlyne complete --task <task-id> --outcome done --text "<one-line result>"
```

or, inside a pi session, the `onlyne_complete{outcome, text}` tool.

- Your `--text` becomes the ledger `out_head`, verbatim: one line, whitespace-collapsed,
  capped at 200 characters. Put the whole answer there. It is the only upward channel.
- `--outcome done|failed|cancelled`. Provable impossibility → `failed`, with the reason in
  `text`. If you fall silent, a fallback still files a receipt from your last assistant
  text — so name the result in that text.
- One completion per task. Inside your session the plugin keeps that record: a second
  `onlyne_complete` for a task it already reported answers `duplicate`, files no report, and the
  process exits once. A hand-run `onlyne complete` carries a fresh `op_id` each call, so the
  ledger reads it as a new frame and appends a second `completion` row beside the first while the
  task's own row keeps the state it settled in. Idempotence keys on `op_id` alone: the same
  `op_id` with the same body answers `duplicate` and replays the first receipt byte for byte, and
  the same `op_id` with a changed body answers `conflict`; each writes no row. Call it once.

## Passing work sideways

```bash
onlyne handoff --to <next-role> --task <task-id> --text "<same task text>"
```

The handoff reads your task's ledger row, mints a child task under `parent_task`, and sets
`hop = parent + 1`. Targets come from your spec entry's `allowed_targets`; any other name
returns `acl_denied` before a row exists. Ring and fan-out shapes live in your prose. The
mechanics here never change.

`onlyne_send{to, text, kind}` covers the same ground from inside a pi session:
`kind:"task"` mints a fresh family, while `kind:"note"` (the default) is free text with no
session on the other side.

## Rules of the ring

- Do not message the supervisor. Results ride completions: the origin recorded in the
  ledger receives them automatically, even from offline queueing. The supervisor grants an
  uplink route for a specific task through the spec, and revokes it the same way.
- Content crosses by reference. Share a file **path** in text; the receiver reads the file.
  Workspace bytes never ride the bus.
- Your local socket answers `who`, `ping`, `watch` from inside the workspace:
  `onlyne who`, `onlyne watch --follow` resolve the `.onlyne/run/s` above your cwd.
- When the server link drops, keep working: your running session still reaches its terminal
  state, and outgoing receipts persist as intents and flush after reconnect. Nothing needs
  your memory to bridge a gap.
- Your pane sits in the operator's session host, addressed as a herdr workspace labelled
  `onlyne:<cluster>` plus a tab named for your role. The operator points the backend at the
  workspace they use by renaming it before sessions spawn: `herdr workspace rename
  <WORKSPACE_ID> onlyne:<cluster>`, then `herdr tab rename <TAB_ID> <role>`. A label that
  differs yields a second workspace, and the client logs a warning naming the label and the
  workspace it created.
