---
name: onlyne
description: Operate the decentralized Onlyne v3 node in Antigravity — send/reply via `msg`, inspect `onlyne://` views with `onlyne_read`, open the interactive `onlyne_panel` GUI, and manage peer trust and resources.
---

# Onlyne v3 in Antigravity

Use this skill whenever the user wants to inspect Onlyne status, read or reply to peer threads, manage key exchanges, inspect Onlyne network resources, or open the Onlyne GUI panel inside Antigravity.

## 1. Quick Inspection

Call `onlyne_read` with:
- `onlyne://` for overview (node ID, URL, unread inbox, threads)
- `onlyne://neighbors` for trusted ledger + mDNS/seed discovered peers
- `onlyne://threads` to list local and joined threads
- `onlyne://inbox` to check unconsumed inbound deliveries
- `onlyne://todo` to inspect collaborative Onlyne work items
- `onlyne://resources` to query registered IM/compute/network resources

## 2. Opening the GUI Panel inside Antigravity

Call the `onlyne_panel` MCP App tool to render the interactive 4-column Onlyne panel (`aux_pane` or `global` view):
- Connect new seed peers by URL and refresh mDNS discovery
- Inspect peer fingerprints, initiate/accept/revoke/rotate keys, and edit acquisition policy (`all` / `segment` / `none`)
- Read full threads, compose/reply via `msg`, join domains, create threads, and toggle Onlyne todos
- Configure `.onlyne/config.toml` visually via Settings

## 3. Sending & Replying via `msg`

Always go through the single agent-facing entry `msg({ delivery_kind, msg, meta })`:

- **Ask or notify a peer**:
  `msg({ delivery_kind: "question", msg: "Can you review PR #42?", meta: { to: "urn:onlyne:node:...", response: "need" } })`
- **Reply in a thread**:
  `msg({ delivery_kind: "answer", msg: "Merged with fixes.", meta: { thread: "<owner>/<id>", in_reply_to: "<sender>/<msg_id>" } })`
- **Stage a draft for user sign-off**:
  `msg({ delivery_kind: "note", msg: "...", meta: { op: "draft", to: "..." } })`
- **Approve/send a draft**:
  `msg({ delivery_kind: "note", msg: "", meta: { op: "send_draft", draft_id: "..." } })`
- **Trust operations**:
  `msg({ delivery_kind: "trust", msg: "verify peer", meta: { op: "trust_begin", to: "<peer-id-or-url>" } })` (also supports `trust_accept`, `trust_revoke`, `trust_rotate`)

## 4. Pairwise Key Exchange Checklist

1. Ensure both nodes have `[node]` identity configured (`.onlyne/config.toml` or via GUI/TUI wizard).
2. Connect the peer's URL or discover via mDNS on LAN.
3. Initiate exchange (`trust` -> `begin` or `[Exchange]` in panel).
4. **Compare both fingerprints out-of-band** with the human before confirming/accepting on the receiving node.
5. Once `exchanged`, messages, threads, and resource handovers authenticate deterministically end-to-end.
