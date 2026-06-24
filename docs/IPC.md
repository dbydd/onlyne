# Onlyne IPC

Onlyne uses newline-delimited JSON over Unix socket and stdio.

## Request envelope

```json
{
  "id": "optional-client-id",
  "op": "ping",
  "channel_id": "optional-channel",
  "conversation_id": "optional-conversation",
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
| `subscribe_events` | Starts async event lines on the same connection. |
| `unsubscribe_events` | Stops event lines for the connection. |
| `send_message` | Requires `channel_id` and `conversation_id`; uses `text` as Markdown by default, optional `raw_text:true` for literal text, legacy optional `format` (`plain` or `markdown`), and optional `attachments`. |
| `reply_message` | Currently same local send path as `send_message`. |
| `fetch_history` | Merged history. |
| `fetch_all_history` | Alias for merged history. |
| `fetch_channel_history` | Requires `channel_id`; optional `conversation_id`. |
| `start_adapter` | Compatibility response; adapters start with daemon startup. |
| `stop_adapter` | Compatibility response. |
| `restart_adapter` | Compatibility response. |

## Event line

Subscribed clients receive lines like:

```json
{"event":true,"type":"inbound_message","data":{"type":"inbound_message","data":{}}}
```

Event types include `inbound_message`, `outbound_message`, `delivery_update`, `adapter_started`, `adapter_stopped`, `adapter_reconnecting`, `adapter_failed`, `history_appended`, `workspace_state_changed`, `warning`, and `error`.
