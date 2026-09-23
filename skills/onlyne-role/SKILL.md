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
- Your session serves this task. Its id is the task id, and a session that finished a task
  takes no second one, so a new task arrives in a fresh session.

## Reporting: completion is the receipt

Report upward by completing the task. The completion row is what the supervisor polls.

```bash
onlyne complete --task <task-id> --outcome done --text "<one-line result>"
```

or, inside a pi session, the `onlyne_complete{outcome, text}` tool.

- Your text becomes the ledger `out_head`: the first 200 grapheme clusters of the completion
  body (`head_preview` in `crates/onlyne-store/src/server.rs`). The `onlyne_complete` tool
  flattens its `text` to one line before it goes, so put the whole answer in that one line.
  It is the only upward channel.
- `--outcome done|failed|cancelled`. Provable impossibility → `failed`, with the reason in
  `text`. The report path depends on your host. A mounted pi session answers through the
  `onlyne_complete` tool, which is the only path to `done`; a call that carries no `text`
  files your last assistant text as the head, so name the result in that text too.
- A mounted pi session whose turn ends without `onlyne_complete` gets the assignment handed
  back to it. The plugin re-injects the same header, task text, and attachment paths (the role
  prose stays out, being already in your context) under the line `[onlyne] your turn ended
  without a completion exit; this task is still open (reminder n of m). Call onlyne_complete
  when it is finished.` The bound is `idleReminders` in `.pi/onlyne.json` — 2 by default, and
  `0` fails the task at the first idle without a completion. The idle that finds the bound
  spent reports the task `failed` with head `no completion after <n> idle reminders` and ends
  the session; a turn that ends with a provider error reports `failed` at once, with that error
  as the head.
- A plain `exec` session carries no plugin: `onlyne complete` is yours to run before you stop.
- A `backend = "acp"` session mounts nothing and needs no `onlyne` command. Its prompt
  ends with an absolute report path your client prepared under the workspace; the last
  action before you stop is that file: one `hop-done:` / `hop-failed:` / `hop-blocked:`
  verdict line, optionally followed by `handoff:` lines your client routes for you. The
  client reads the file, settles the task, files the receipt, and passes the handoffs on.
  Write and check the file with the `onlyne report` verbs, or read
  `skills/onlyne-role-payload-v2/SKILL.md` for the whole grammar before you write one.
- One completion per task. Inside your session the plugin keeps that record: a second
  `onlyne_complete` for a task it already reported answers `duplicate` and files no report. The
  process leaves once, at the completion the plugin acked. A hand-run `onlyne complete` carries
  a fresh `op_id` each call, so the ledger reads it as a new frame and appends a second
  `completion` row beside the first, and the task's own row keeps the state it settled in.
  Idempotence keys on `op_id` alone: the same
  `op_id` with the same body answers `duplicate` and replays the first receipt byte for byte, and
  the same `op_id` with a changed body answers `conflict`; each writes no row. Call it once.

## Passing work sideways

```bash
onlyne handoff --to <next-role> --task <task-id> --text "<same task text>"
```

The handoff reads the deepest row of your task family, mints a child task under
`parent_task`, and sets `hop = parent + 1`. The server gates `--to` on your spec entry's
`allowed_targets`, and any other name returns `acl_denied` before a row exists. Ring and
fan-out shapes live in your prose. The mechanics here never change.

`onlyne_send{to, text, kind, image}` covers the same ground from inside a pi session:
`kind:"task"` mints a fresh family; `kind:"note"` (the default) is free text with no session
on the other side.

## Rules of the ring

- Do not message the supervisor. Results ride completions: the origin recorded in the
  ledger receives them automatically, even from offline queueing. The supervisor grants an
  uplink route for a specific task through the spec, and revokes it the same way.
- Content crosses by reference. Share a file **path** in text; the receiver reads the file.
  One part rides the bus: `onlyne_send{..., image}` attaches a single image, capped at 2 MiB
  of decoded bytes, in png, jpeg, gif, or webp.
- Your local socket answers `who` and `ping` in place, and every other verb it carries travels
  on to the server through your client's link. `onlyne who` and `onlyne ping` resolve the
  `.onlyne/run/s` above your cwd, and `onlyne watch --follow` resolves that same path and
  subscribes your client's own link to the event stream.
- When the server link drops, keep working: your running session still reaches its terminal
  state, and outgoing receipts persist as intents and flush after reconnect. Nothing needs
  your memory to bridge a gap.
- In a herdr-hosted workspace your pane sits in the operator's session host, addressed as a
  herdr workspace labelled
  `onlyne:<cluster>` plus a tab named for your role. The operator points the backend at the
  workspace they use by renaming it before sessions spawn: `herdr workspace rename
  <WORKSPACE_ID> onlyne:<cluster>`, then `herdr tab rename <TAB_ID> <role>`. A label that
  differs yields a second workspace, and the client logs a warning naming the label and the
  workspace it created.
