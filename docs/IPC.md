# Onlyne IPC

Onlyne uses newline-delimited JSON over Unix socket and stdio.

## Request envelope

```json
{
  "id": "optional-client-id",
  "op": "ping",
  "channel_id": "optional-channel",
  "text": "optional text, treated as Markdown by default",
  "raw_text": false,
  "format": "markdown",
  "attachments": [],
  "limit": 100
}
```

## Response envelope

Success:

```json
{"id":"1","ok":true,"data":{}}
```

Error:

```json
{"id":"1","ok":false,"error":{"code":"error","message":"..."}}
```

Malformed JSON uses `code=bad_json`.

## Operations

| Operation | Notes |
| --- | --- |
| `ping` | Health check. |
| `status` | Workspace, socket, channels. |
| `list_channels` | Stored channel health rows. |
| `list_conversations` | Optional `channel_id`. |
| `subscribe_events` | Starts async event lines on the same connection. Optional `priority` (u32 tier, default 0) and `consume_timeout_ms` (per-tier verdict wait, default 150). Events carry `event_seq`. See tiered delivery below. |
| `unsubscribe_events` | Stops event lines for the connection. |
| `consume` | Requires `event_seq`. Marks the referenced event consumed so lower-priority tiers do not receive it. Replies `{"ok":true,"data":{"consumed":seq}}`. |
| `shutdown` | Requests the workspace-local daemon to exit after responding. Used by `onlyne stop` / `onlyne restart`. |
| `send_message` | Requires `channel_id`; Onlyne routes to that channel's configured or `/handshake`-bound `bind_conversation_id`. Uses `text` as Markdown by default, optional `raw_text:true` for literal text, legacy optional `format` (`plain` or `markdown`), and optional `attachments`. |
| `reply_message` | Currently same local send path as `send_message`. |
| `loopback` | Injects a local inbound activation message on channel `loopback`; optional `text`, `raw_text`, `format`, and `attachments`. |
| `swarm_ready` | Swarm handshake: pi-onlyne swarm mode reports readiness. `text` carries JSON `{workspace, terminal_handle}`. Recorded in history and published as a `workspace_state_changed` event carrying the body, so the scheduler can match a pending task. |
| `fetch_history` | Merged history. |
| `fetch_all_history` | Alias for merged history. |
| `fetch_channel_history` | Requires `channel_id`; returns that channel's configured conversation history. |
| `start_adapter` | Compatibility response; adapters start with daemon startup. |
| `stop_adapter` | Compatibility response. |
| `restart_adapter` | Compatibility response. |

## Event line

Subscribed clients receive lines like:

```json
{"event":true,"type":"inbound_message","data":{"type":"inbound_message","data":{}}}
```

Event types include `inbound_message`, `outbound_message`, `delivery_update`, `adapter_started`, `adapter_stopped`, `adapter_reconnecting`, `adapter_failed`, `history_appended`, `workspace_state_changed`, `warning`, and `error`.

## Tiered priority delivery (swarm)

Subscribers register a `priority` tier with `subscribe_events`. Each event is
relayed highest-tier-first with a fresh `event_seq` tag:

1. The daemon fans the event out to every member of the highest tier.
2. It waits `consume_timeout_ms` for any member to reply
   `{"op":"consume","event_seq":N}`.
3. If a tier consumed the event, lower tiers never see it. Otherwise the next
   lower tier receives it with its own `event_seq`, repeating step 2.

Connections without `priority` sit at tier 0 and keep the legacy
broadcast-to-all behavior among themselves. The swarm scheduler subscribes at
`priority = 4294967295` so it sees swarm traffic first and consumes what it
claims; the TUI debug subscription uses `4294967294`.
