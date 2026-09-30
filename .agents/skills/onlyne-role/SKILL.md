---
name: onlyne-role
description: Use when acting as a role inside an Onlyne cluster — a session assigned a task by the server, needing the correct handoff, completion, and reporting discipline.
---

# Onlyne Role

You are one role in an Onlyne cluster. A task arrives as an injected message, and the work
you are handed belongs to it. Do the work, then report it with a completion.

## How work reaches you

- The task arrives as a user-role injection that names its source: a first line
  `From <role>:`, the body, and then the paths of any attachments written for you under
  `<workspace>/.onlyne/tmp/attachments/`. Your role prose comes from the cluster spec and is
  already in your context: pi takes it as a system-prompt section, and an ACP session reads
  it from the client's block in the workspace's `AGENTS.md`.
- The `{task}` placeholder in the role's `[client.runtime] command` is the task **id**,
  never the body. Argv holds no payload.
- Whether your session serves one delivery or several is the workspace's
  `[client.session] scope`: `oneshot` (the default) opens a session per delivery and closes
  it when that delivery settles, `task` keeps the session for the family's next delivery,
  and `role` keeps a pool and hands the next delivery to whichever session has waited
  longest. An idle session keeps its row, and this client may release its process and bring
  it back for the next delivery.

## Reporting: completion is the receipt

Report upward by completing the task. The completion row is what the supervisor polls.

Inside a pi session the plugin tool is the path:

```
onlyne_complete{outcome, summary, details, files}
```

The CLI verbs that speak for a role — `send`, `reply`, `handoff`, `complete`, `ack`, `reject`,
`control` — refuse a call missing either `--force` or
`--yes-i-am-supervisor-not-other-role`, because that door stands outside a plugin session and a
verdict written there answers for a session the plugin is still serving. An `exec` session mounts
no plugin, so it runs the CLI form and declares itself with both flags:

```bash
onlyne complete --task <task-id> --outcome done --summary "<one-line result>" \
  [--details "<the full result>" --file <absolute path>] \
  --force --yes-i-am-supervisor-not-other-role
```

- Your `summary` becomes the ledger `out_head`: the first 200 grapheme clusters of the
  completion body (`head_preview` in `crates/onlyne-store/src/server.rs`), flattened to one
  line before it goes. It is the display line everything upstream reads, so put the result
  there; a `summary` that carries nothing files your last assistant text as the head instead.
- `outcome` is `done`, `failed`, `cancelled`, or `blocked`. Provable impossibility →
  `failed`, with the reason in `summary`; something outside this session that stops the work
  → `blocked`. A mounted pi session answers through the `onlyne_complete` tool, which is the
  only path to `done`.
- `details`, at or under 64 KiB, carries the full result and `files` names the absolute
  paths it rests on; both travel with the completion as they stand.
- A plain `exec` session carries no plugin: `onlyne complete` is yours to run before you
  stop, with both supervisor flags above.
- An ACP session mounts the same three tools through `onlyne mcp`, the MCP server its
  client attaches to the session, so it needs no `onlyne` command of its own. The names
  and arguments are the ones above, and a refused call comes back as the tool result's
  error text, in the client's own words. Nothing about the task is yours to name: the
  client holds the task, and a call names only the recipient, the text, or the outcome.
- A turn that ends without `onlyne_complete` gets one nudge in your input: `If this task
  is finished, report it with onlyne_complete; if something is missing, say what.` A
  second turn that ends without one settles the delivery — a `oneshot` task becomes
  `blocked`, and a `task` or `role` session goes idle and the board reads "waiting".
- One completion per task. Inside your session the plugin keeps that record: a second
  `onlyne_complete` for a task it already reported files no further report and answers the
  same `reported <outcome>` line. The process leaves once, at the completion the plugin
  acked. A hand-run `onlyne complete` carries a fresh `op_id` each call, so the ledger reads
  it as a new frame and appends a second `completion` row beside the first, and the task's
  own row keeps the state it settled in. Idempotence keys on `op_id` alone: the same `op_id`
  with the same body answers `duplicate` and replays the first receipt byte for byte, and
  the same `op_id` with a changed body answers `conflict`; each writes no row. Call it once.

## Passing work sideways

Inside a pi session, `onlyne_handoff` hands this task on:

```
onlyne_handoff{to: "<next-role>", text: "<same task text>", image: "<path, optional>"}
```

The host mints the child: it names this task as `parent_task`, sits one hop deeper, and carries the
family's metadata — the family root id, the hop budget, the origin, the deadline, and the labels.
A task that meets the family's hop budget is the one that keeps the work: a `handoff` that would
sit over the budget is refused, and the refusal names the budget it would break.

An `exec` session runs the CLI form of the same step, with both supervisor flags:

```bash
onlyne handoff --to <next-role> --task <task-id> --text "<same task text>" \
  --force --yes-i-am-supervisor-not-other-role
```

`onlyne_send{to, text, kind, image}` starts a fresh family instead: `kind:"task"` mints a task with
no `parent_task` and `hop = 0`, `kind:"note"` (the default) is free text with no session on the
other side, and `image` attaches one image, 2 MiB of decoded bytes. The server gates the target of
every route on your spec entry's `allowed_targets`, and any other name returns `acl_denied` before
a row exists. Ring and fan-out shapes live in your prose. The mechanics here never change.

That one list is your obligation as well as your reach: before you report a terminal outcome
your session must have delivered to every *downstream* role the list names. The role that handed
you the task is never one of them — completing is itself the delivery back to it — so a
self-addressed entry, or a ring's return edge, asks nothing of you. A completion that still owes
a role is refused, and the refusal names the roles you have not reached yet and the ones you
have; an entry that names no target owes nothing. There is no separate switch to declare.

## Rules of the ring

- Do not message the supervisor. Results ride completions: the origin recorded in the
  ledger receives them automatically, even from offline queueing. The supervisor grants an
  uplink route for a specific task through the spec, and revokes it the same way.
- Content crosses by reference. Share a file **path** in text; the receiver reads the file.
  One part rides the bus: `onlyne_send{..., image}` attaches a single image, capped at 2 MiB
  of decoded bytes, in png, jpeg, gif, or webp.
- Your local socket answers `who` and `ping` in place, and every other verb it carries travels
  on to the server through your client's link. `onlyne who` and `onlyne ping` resolve the socket
  of the tree above your cwd, and `onlyne watch --follow` resolves that same path and
  subscribes your client's own link to the event stream.
- When the server link drops, keep working: your running session still reaches its terminal
  state, and outgoing receipts persist as intents and flush after reconnect. Nothing needs
  your memory to bridge a gap.
