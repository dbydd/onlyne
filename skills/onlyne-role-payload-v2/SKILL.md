---
name: onlyne-role-payload-v2
description: Use when writing or checking an Onlyne acp session's closing report (结项文件 / payload) under `.onlyne/out` — the one-line verdict with optional `handoff:` lines, its format validation (`onlyne report check` / `validate`), or handing this turn's result on to another role. Triggers: 写 .onlyne/out 结项文件, payload 报告, handoff 转手, 格式校验, acp 结项.
---

# Onlyne Role: payload-v2 Closing Report

A `backend = "acp"` session mounts no CLI: it closes its task through one
file. The prompt you received ends with a directive naming the absolute report
path (`<workspace>/.onlyne/out/<task-id>.md`) and printing the grammar below.
Your client reads that file after your turn stops, settles the task from it,
and routes any handoffs you asked for.

## When to read this skill

- You are an acp role and the directive's path is in your prompt: write the report
  in exactly this shape.
- You want to check a report you already wrote before you stop.
- You want to pass follow-up work to another role from the same turn.

## The grammar

One file = one verdict line first, then zero to eight `handoff:` lines. Lines
starting with `#` are comments; blank lines are ignored. Any other line voids
the whole report — nothing routes, the task is cancelled, the file stays on
disk for you to rewrite.

```text
hop-done: <the result in one line>
hop-failed: <why the task failed, one sentence>
hop-blocked: <what the task is waiting on, one sentence>
handoff: <target role> | <one line for that role>
```

- `hop-done:` — the work finished; the text becomes the ledger head.
- `hop-failed:` — the work failed; the text is both head and fault reason.
- `hop-blocked:` — the work waits on something outside this task; the text is the
  reason. A blocked report never hands anything on: the work is not finished.
- `handoff:` — ask your client to deliver one line to another role. The `|` part
  is optional: without it the recipient gets the verdict line's own text. The
  target must be a role your spec entry may address (`allowed_targets`); an
  invalid target is recorded as `handoff_denied` and changes nothing else —
  your verdict still settles.
- At most 16 lines total, at most 8 handoff lines. A single verdict line is a
  valid report (that is the v1 shape).

Example — a build role that finished and wants a review pass:

```text
hop-done: v2 report path landed with the CLI verbs
handoff: reviewer | skim crates/onlyne-cli/src/report.rs before merge
handoff: builder
```

`builder` gets the verdict line itself; `reviewer` gets its own one-liner.

## Get the path, then write

```bash
onlyne report path --task <task-id>          # the report path plus the session's log paths
```

Write the file by creating a temporary name in the same directory and renaming
it into place, so no reader ever sees a half-written report. Or skip assembly
entirely and let the CLI build a valid file from its parts:

```bash
onlyne report write --task <task-id> --verdict done --head "one-line result" \
  --handoff "reviewer|skim before merge" --handoff builder
```

`write` constructs only valid reports (it refuses multi-line heads and empty
verdicts), writes atomically, and prints the final path.

## Check before you stop

```bash
onlyne report check --task <task-id>         # verdict + every handoff, exit 0
onlyne report check --path <file>            # same, naming the file directly
echo 'hop-blocked: waiting on the registry' | onlyne report validate --from -
```

- Exit 0 prints what your client will do with the file: the kind, the head or
  reason, and every handoff line.
- Exit 2 is every answer the verb cannot give as valid: an invalid file prints
  the exact reason with the line number it broke
  (`onlyne: line 3: unknown report prefix "hop-parked"`) and the whole grammar
  on stderr; a file that is not there answers `absent` and an unreadable one
  names the OS reason, both on stderr. Exit 3 belongs to socket resolution and
  never answers a `report` verb. Fix the file and check again — an invalid
  report cancels the task with zero handoffs, and the file stays where it is,
  so a rewrite is enough.

Never leave the turn stopping on a report that `check` refuses.
