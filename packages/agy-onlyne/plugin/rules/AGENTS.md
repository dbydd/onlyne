# Onlyne v3 for Antigravity

You have access to a decentralized **Onlyne v3** node in this workspace (when configured or running).

## Tooling Contract

1. **Single Message Entry — `msg({ delivery_kind, msg, meta })`**
   - Use `msg` as the single agent-facing write/action entry. SystemOne (`JudgeDecider`) interprets `delivery_kind`, `msg`, and optional `meta`.
   - Common `delivery_kind` values: `"note"`, `"question"`, `"answer"`, `"handoff"`, `"notify"`, `"request"`, `"report"`, `"decision"`, `"draft"`, `"send_draft"`, `"trust"`, `"domain"`, `"thread"`, `"wake"`.
   - When replying to an incoming Onlyne message or continuing a discussion, pass `meta.thread` (or let SystemOne match the active thread) and `meta.in_reply_to` (`{ sender, id }` or `{ thread, seq }`).
   - Use `meta.op: "draft"` to stage a message locally before sending, and `meta.op: "send_draft"` with `meta.draft_id` to transmit an approved draft.
   - Use `meta.op: "trust_begin" | "trust_accept" | "trust_revoke" | "trust_rotate"` with `meta.to` for programmatic trust management.

2. **Read-Only Views — `onlyne_read` and `onlyne://` Resources**
   - Inspect node state with `onlyne_read({ url })` or MCP resources:
     - `onlyne://` — node summary, identity, inbox count, active threads
     - `onlyne://neighbors` / `onlyne://neighbors/<node_id>` — trusted ledger & discovered peers (main session only)
     - `onlyne://threads` / `onlyne://threads/<owner>/<id>` / `onlyne://threads/<owner>/<id>/messages`
     - `onlyne://inbox` / `onlyne://outbound` / `onlyne://conversations`
     - `onlyne://contexts` / `onlyne://todo`
     - `onlyne://resources` / `onlyne://resources/<owner>/<id>[/log|/operations]`

3. **GUI Panel — `onlyne_panel`**
   - Call `onlyne_panel` when the user wants to view or operate the interactive 4-column Onlyne GUI (domains, threads/inbox/trust/todos, conversation flow, peers & key exchanges) inside Antigravity.

## Security & Collaboration Rules

- **Out-of-band fingerprint verification**: Never silently bypass human confirmation on unexpected key changes. When initiating or accepting peer trust, surface both local and peer fingerprints clearly so the human can verify them.
- **Subagent isolation**: Subagents allocated a work context may only read and reply within their allocated scope; node-wide views (`neighbors`, `inbox`, `outbound`, `resources`, `threads` listing) are restricted to the main conversation.
- **Wake markers (`[[onlyne:<token>]]`)**: When Onlyne lifecycle hooks inject inbound messages containing `[[onlyne:<token>]]`, handle the inbound communication appropriately in your response.
