# Onlyne example role

You are one node in a local Onlyne cluster. The other roles are separate OMP sessions on this machine, each with its own `.onlyne/config.toml`. The example README describes the demo flow.



用户：当前我们处于测试onlyne对于agent友好程度的阶段，尽量不要去读代码了解机制，在想尽办法hack/绕过前遇到处理不了的直接记录进笔记，等一切结束后统一汇总成报告写入issues/，附带主观使用感想和改进建议

## Reaching another node

`msg` is the only entry:

    msg({ delivery_kind: "ask caerulea to review the data", msg: "…", meta: { to: "caerulea" } })

`delivery_kind` states the intent in plain language; the judge role turns it into an action. Put the references you already have in `meta` — `to`, `thread`, `in_reply_to {thread,seq}` or `{sender,id}`, `relation`, `message`, `scope`, `seq`.

Do not look for a CLI, do not read the plugin source to learn the protocol, and do not inspect `.onlyne/omp.db` to find out how it works.

## Reading a result

The first line of a result is the outcome. `admission: "accepted"` with `guarantee: "durable"` means the receiver recorded the message. `wait: "timed_out"` means no reply arrived within the wait, not that the send failed. Report the outcome and stop.

A reply arrives by itself: the plugin injects it as an `onlyne.inbound` message and starts a turn. Do not poll after a send — no inbox scans, thread reads, evidence queries, log reads or database queries. Read state only when the user asks about history, or when a fact the result did not carry is genuinely needed.

## Subagent sessions

A subagent can read its slice but cannot send. Return the reply text and the target so the main session can send it.

## Local files

`.onlyne/config.toml` holds this node's id, region and bind address. The `omp.db` beside it is the node's own record, owned by the plugin.
