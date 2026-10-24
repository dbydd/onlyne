# Onlyne in this project

A starting point for a project that hosts an Onlyne node. Copy what you need into the project's own `AGENTS.md`; the wording assumes the `omp-onlyne` plugin is loaded.

## Reaching another node

`msg` is the only entry:

    msg({ delivery_kind: "ask reviewer to check the data", msg: "…", meta: { to: "reviewer" } })

`delivery_kind` states the intent in plain language; the judge role turns it into an action. Put the references you already have in `meta` — `to`, `thread`, `in_reply_to {thread,seq}` or `{sender,id}`, `relation`, `message`, `scope`, `seq`.

The tool is the whole surface. Do not look for a CLI, do not read the plugin source to learn the protocol, and do not open the node database to find out how it works.

## Reading a result

The first line of a result is the outcome. `admission: "accepted"` with `guarantee: "durable"` means the receiver recorded the message. `wait: "timed_out"` means no reply arrived within the wait, not that the send failed. Report the outcome and stop.

A reply arrives by itself: the plugin injects it as an `onlyne.inbound` message and starts a turn. Do not poll after a send — no inbox scans, thread reads, evidence queries, log reads or database queries. Read state when the user asks about history, or when a fact the result did not carry is genuinely needed.

## Subagent sessions

A subagent can read its slice but cannot send. Return the reply text and the target so the main session can send it.

## Local files

`.onlyne/config.toml` holds the node id, region and bind address. The `omp.db` beside it is the node's own record, owned by the plugin; treat it as runtime state, not documentation.
